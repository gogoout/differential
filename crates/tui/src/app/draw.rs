//! Every `draw_*`, and the row composition behind them.
//!
//! Draw is read-only by construction: it takes `&self`. Anything a frame needs
//! that costs more than reading a field belongs in `state`, computed once when
//! the thing it depends on changed.

use std::collections::HashSet;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::rows::{Border, Fill, Gutter, Half, RowKind};
use crate::theme::Theme;
use crate::vendor::text_utils::{
    drop_columns, slice_pairs, split_pairs_at_ranges, truncate_or_pad_spans, wrap_pairs,
};

use super::text::{
    Hint, Ink, basename, counts_columns, elide_head, file_list_rows, findings_rows, findings_skip,
    joined, pad_to_width, plain, truncate_width,
};
use super::*;
use crossterm::event::KeyCode;

impl App {
    // ------------------------------------------------------------- drawing

    pub fn draw(&self, frame: &mut Frame) {
        // The theme's own ground, under everything. Without it the terminal's
        // background shows through wherever nothing else paints, which is most
        // of the screen — and a light palette over a dark terminal is pale ink
        // on black.
        frame.render_widget(
            ratatui::widgets::Block::default().style(self.theme.ground()),
            frame.area(),
        );
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
            Mode::Editing {
                editor: textarea, ..
            } => {
                let area = composer_area(panes.body, textarea);
                clear_to_ground(frame, &self.theme, area);
                frame.render_widget(&**textarea, area);
                // The keys go INSIDE the box, on its last row, where a footer
                // belongs — the title says what you are annotating.
                let hints = composer_footer();
                let row = footer_row(area);
                let x = centered_x(row, hints_width(&hints));
                frame.render_widget(
                    footer_line(&self.theme, &hints),
                    Rect {
                        x,
                        width: row.width.saturating_sub(x - row.x),
                        ..row
                    },
                );
            }
            Mode::Help(_) => {
                // As tall as its own table: a fixed height cut the footer off
                // the first time the table grew a row.
                let lines = help_lines(&self.theme, &self.help_sections());
                let height = lines.len() as u16 + 2;
                let area = centered_rect(panes.body, 74, height);
                self.float(frame, area, " help ", Paragraph::new(lines));
            }
            Mode::Notice { title, text } => {
                // Wrapped, and as tall as it needs: an error is read once and
                // in full, or it is not read at all. The rows are counted with
                // the same word wrap the paragraph draws with, so the box and
                // its text cannot disagree; borders, two blank rows and the
                // footer make five more.
                let width = panes.body.width.saturating_sub(6).clamp(20, 100);
                let rows = wrapped_rows(text.lines(), usize::from(width.saturating_sub(4)));
                let height = u16::try_from(rows + 5).unwrap_or(u16::MAX);
                let mut lines: Vec<Line> = vec![Line::from("")];
                lines.extend(text.lines().map(|l| {
                    Line::from(Span::styled(
                        format!(" {l}"),
                        Style::default().fg(self.theme.context_fg),
                    ))
                }));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    " press any key to close",
                    Style::default().fg(self.theme.gutter_fg),
                )));
                self.float(
                    frame,
                    centered_rect(panes.body, width, height),
                    &format!(" {title} "),
                    Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }),
                );
            }
            Mode::DeleteComment { own } => {
                let lines = self.delete_comment_lines(own);
                let area = delete_comment_area(panes.body, lines.len());
                self.float(frame, area, " delete ", Paragraph::new(lines));
            }
            Mode::Publish { plan } => {
                let lines = self.publish_lines(plan);
                let area = publish_area(panes.body, lines.len());
                self.float(frame, area, " publish ", Paragraph::new(lines));
            }
            Mode::Findings {
                entries,
                selected,
                scroll,
                confirming,
            } => {
                let orphans = entries.iter().filter(|e| e.orphaned).count();
                let threads = entries.iter().filter(|e| e.thread).count();
                let notes = entries.len() - threads;
                let rules = section_rules(entries);
                let body_rows = panes.body.height as usize;
                let area = findings_modal_area(panes.body, entries.len(), rules.len());
                let inner_w = area.width.saturating_sub(2) as usize;
                // The same number `j`/`k` scroll against, from the same
                // function, so the window a list moves in is the window it is
                // drawn in.
                let inner_h = findings_rows(entries.len(), rules.len(), body_rows);

                let dim = Style::default().fg(self.theme.gutter_fg);
                // A BUDGET, not a measurement. `{:<width$}` pads and never
                // truncates, so a path longer than the column used to overflow
                // it and starve every note in the list of its row. A third of
                // the box leaves the notes readable.
                let at_col = entries
                    .iter()
                    .map(|e| UnicodeWidthStr::width(e.at.as_str()))
                    .max()
                    .unwrap_or(0)
                    .min(inner_w / 3);
                let mut lines: Vec<Line> = Vec::new();
                for (i, e) in entries.iter().enumerate() {
                    if rules.contains(&i) {
                        let label = match e.section() {
                            1 => "review threads",
                            _ => "orphaned",
                        };
                        lines.push(Line::from(Span::styled(
                            format!(
                                " ── {label} {} ",
                                "─".repeat(inner_w.saturating_sub(label.len() + 6))
                            ),
                            dim,
                        )));
                    }
                    let on = i == *selected;
                    let base = Style::default().fg(if e.orphaned || e.resolved {
                        self.theme.gutter_fg
                    } else {
                        self.theme.context_fg
                    });
                    let style = if on {
                        base.bg(self.theme.selected_bg).add_modifier(Modifier::BOLD)
                    } else {
                        base
                    };
                    let bg = |st: Style| {
                        if on {
                            st.bg(self.theme.selected_bg)
                        } else {
                            st
                        }
                    };
                    let moved = match (e.moved, e.published, e.resolved) {
                        (true, _, _) => " (moved)",
                        (_, true, _) => " (published)",
                        (_, _, true) => " (resolved)",
                        _ => "",
                    };
                    let at_text = elide_head(&e.at, at_col);
                    // Padded by DISPLAY width, not by `{:<width$}`, which
                    // counts chars — one wide character in a path and the
                    // column stopped lining up.
                    let pad = at_col.saturating_sub(UnicodeWidthStr::width(at_text.as_str()));
                    let at = format!("  {at_text}{}  ", " ".repeat(pad));
                    // Cut the note, not the box: a body that reached the border
                    // was chopped mid-word against it and read as broken. Every
                    // one of the three widths here is a DISPLAY width — they
                    // were a display width, a byte count and a char count, and
                    // any wide character made the row's arithmetic wrong.
                    let room = inner_w.saturating_sub(
                        UnicodeWidthStr::width(at.as_str()) + UnicodeWidthStr::width(moved) + 1,
                    );
                    let body = truncate_width(&e.body, room);
                    let mut line = Line::from(vec![
                        Span::styled(at, bg(dim)),
                        Span::styled(body, style),
                        Span::styled(moved.to_string(), bg(dim)),
                    ]);
                    if on {
                        pad_to_width(&mut line, inner_w, self.theme.selected_bg);
                    }
                    lines.push(line);
                }
                // A rule is a row too, so scrolling counts drawn rows.
                let skip = findings_skip(*scroll, &rules);
                let shown: Vec<Line> = lines.into_iter().skip(skip).take(inner_h).collect();

                // The keys go in a footer inside the box, as the composer's
                // do: the confirmation needs that row anyway, and a title
                // carrying four keys is longer than the box.
                // Counts what `y` would take: the local notes. A published
                // note and a thread are listed here but are not up for this.
                let local = entries.iter().filter(|e| !e.thread && !e.published).count();
                // The same number the status after `y` reports: every record on
                // the request, whether it is listed as a note or as its thread.
                let kept = self.published_count();
                let hints = match *confirming {
                    true => findings_question(local, kept),
                    false => self.modal_footer(),
                };
                let footer = footer_line(&self.theme, &hints);
                let mut title = format!(" findings · {notes} ");
                if threads > 0 {
                    title.push_str(&format!("· threads · {threads} "));
                }
                if orphans > 0 {
                    title.push_str(&format!("· {orphans} orphaned "));
                }
                clear_to_ground(frame, &self.theme, area);
                frame.render_widget(
                    Paragraph::new(shown).block(pane(&self.theme, title, true)),
                    area,
                );
                frame.render_widget(Paragraph::new(footer), footer_row(area));
            }
            Mode::FileList {
                entries,
                selected,
                scroll,
            } => {
                let body_rows = panes.body.height as usize;
                // Window before building, and by the same number `j`/`k`
                // scroll against: the surplus lines used to be built and then
                // silently dropped off the bottom of the box.
                let inner_h = file_list_rows(entries.len(), body_rows);

                let (add_w, del_w, lead) = counts_columns(entries);
                let area = file_list_modal_area(panes.body, entries);
                let inner_w = area.width.saturating_sub(2) as usize;
                let path_col = inner_w.saturating_sub(lead);

                let lines: Vec<Line> = entries
                    .iter()
                    .enumerate()
                    .skip(*scroll)
                    .take(inner_h)
                    .map(|(i, e)| {
                        let mark = if e.reviewed { "✓" } else { " " };
                        let mut style = Style::default().fg(self.theme.context_fg);
                        if i == *selected {
                            style = style
                                .bg(self.theme.selected_bg)
                                .add_modifier(Modifier::BOLD);
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
                            Span::styled(format!("+{:<add_w$}", e.adds), on(self.theme.add_fg)),
                            Span::styled(format!("−{:<del_w$} ", e.dels), on(self.theme.del_fg)),
                            // Whole when it fits, and cut at its HEAD when it
                            // does not, so the name survives whatever the
                            // directories above it cost.
                            Span::styled(elide_head(&e.path, path_col), style),
                        ])
                    })
                    .collect();
                clear_to_ground(frame, &self.theme, area);
                frame.render_widget(
                    Paragraph::new(lines).block(pane(
                        &self.theme,
                        // The keys are the table's too, drawn in the title
                        // because this box has no spare row for a footer.
                        format!(" files — {} ", plain(&self.modal_hints())),
                        true,
                    )),
                    area,
                );
            }
            Mode::Normal => {}
        }
    }

    pub(super) fn draw_groups(&self, frame: &mut Frame, area: Rect) {
        let inner_h = area.height.saturating_sub(2) as usize;
        let inner_w = area.width.saturating_sub(2) as usize;
        let selected = self.selected_entry();

        // Entries render as blocks of lines, so scrolling counts ROWS, not
        // entries; keep the whole selected block in view.
        let mut blocks: Vec<Vec<Line>> = match self.view_mode {
            ViewMode::Groups => {
                // Both hoisted out of the loop. `edge_span` takes no argument
                // and depends only on the selection, so it gave every group
                // the same answer while costing a scan of every group to do
                // it. The marks are one set, read once, instead of a map
                // lookup and a set lookup per hunk of every group per frame.
                let span = self.edge_span();
                let reviewed = &self.reviewed;
                (0..self.groups().len())
                    .map(|i| self.group_lines(i, i == selected, inner_w, span, reviewed))
                    .collect()
            }
            ViewMode::Files => {
                let reviewed = &self.reviewed;
                let guides = tree_guides(&self.tree);
                (0..self.tree.len())
                    .map(|i| self.tree_lines(i, i == selected, reviewed, &guides[i]))
                    .collect()
            }
        };
        // The selection reads as a row, not as highlighted text: pad its lines
        // out to the pane so the background runs to the right edge.
        if let Some(block) = blocks.get_mut(selected) {
            for line in block.iter_mut() {
                pad_to_width(line, inner_w, self.theme.selected_bg);
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
        let block = pane(&self.theme, title, self.focus == Focus::Groups);
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
    pub(super) fn edge_span(&self) -> (usize, usize) {
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
    ///
    /// `width` is the pane's inner width, which the role pill needs: it hangs
    /// off the RIGHT edge, so it is the one thing here whose position depends
    /// on how wide the pane is.
    pub(super) fn group_lines(
        &self,
        idx: usize,
        selected: bool,
        width: usize,
        (lo, hi): (usize, usize),
        reviewed: &HashSet<usize>,
    ) -> Vec<Line<'static>> {
        let g = &self.groups()[idx];
        let relation = self.relation_to_selected(idx);
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
        let head_style = Style::default().fg(self.theme.gutter_fg);
        let tail_glyph = if idx >= lo && idx < hi { "│ " } else { "  " };
        let done = !g.hunks.is_empty() && g.hunks.iter().all(|h| reviewed.contains(&h.index()));
        // "?" rather than a tier letter: the back-fill was never classified.
        let tier = if g.unclassified {
            "?"
        } else {
            Theme::effort_glyph(g.effort)
        };
        let bg = |st: Style| {
            if selected {
                st.bg(self.theme.selected_bg).add_modifier(Modifier::BOLD)
            } else {
                st
            }
        };
        let dim = bg(Style::default().fg(self.theme.gutter_fg));

        let mut lines = vec![Line::from(vec![
            Span::styled(head_glyph.to_string(), head_style),
            Span::styled(
                // The id is what `after:` references, so it has to be visible.
                format!("{:>3} ", g.id),
                bg(Style::default().fg(self.theme.gutter_fg)),
            ),
            Span::styled(
                format!("{tier} "),
                bg(self
                    .theme
                    .effort_style(g.effort)
                    .add_modifier(Modifier::BOLD)),
            ),
            Span::styled(
                g.label.clone(),
                bg(Style::default().fg(if done {
                    self.theme.reviewed_fg
                } else {
                    self.theme.context_fg
                })),
            ),
            Span::styled(
                if done { "  ✓" } else { "" }.to_string(),
                bg(Style::default().fg(self.theme.reviewed_fg)),
            ),
        ])];

        let mut counts = vec![
            Span::styled(
                tail_glyph.to_string(),
                Style::default().fg(self.theme.gutter_fg),
            ),
            Span::styled(format!("   {} files  ", g.n_files), dim),
            Span::styled(
                format!("+{}", g.counts.adds),
                bg(Style::default().fg(self.theme.add_fg)),
            ),
            Span::styled(" ", dim),
            Span::styled(
                format!("−{}", g.counts.dels),
                bg(Style::default().fg(self.theme.del_fg)),
            ),
        ];
        // The ordering role is a fact about the group, like the class on a hunk
        // header — so it wears the same pill, in the muted colours, rather than
        // trailing off the line as dim text.
        //
        // Against the RIGHT edge, so the roles line up in a column of their
        // own. Trailing the counts, they started at a different place on every
        // row — a word you can only read by finding it first.
        if let Some(r) = g.role {
            let (fg, pill_bg) = self.theme.pill();
            let badge: Vec<Span> = pill(
                vec![(fg, differential_engine::plan::role_name(r).to_string())],
                pill_bg,
            )
            .into_iter()
            .map(|(st, t)| Span::styled(t, st))
            .collect();
            let used: usize = counts
                .iter()
                .chain(&badge)
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum();
            counts.push(Span::styled(
                " ".repeat(width.saturating_sub(used).max(1)),
                dim,
            ));
            counts.extend(badge);
        }
        lines.push(Line::from(counts));
        if !g.depends_on.is_empty() {
            // Every id reads the same. A dependency the ordering could not
            // honour used to wear a `↓` and a colour of its own, which put a
            // warning on the row for something the reader can do nothing about
            // — and the connector already shows it, by running DOWN from the
            // selected group instead of up.
            let mut spans = vec![
                Span::styled(
                    tail_glyph.to_string(),
                    Style::default().fg(self.theme.gutter_fg),
                ),
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
    pub(super) fn tree_lines(
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
                st.bg(self.theme.selected_bg).add_modifier(Modifier::BOLD)
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
            self.theme.reviewed_fg
        } else {
            self.theme.context_fg
        }));
        let dim = bg(Style::default().fg(self.theme.gutter_fg));

        match &entry.kind {
            TreeKind::Dir { path } => {
                let glyph = if self.collapsed.contains(path) {
                    "▸"
                } else {
                    "▾"
                };
                let name = basename(path).to_string();
                vec![Line::from(vec![
                    Span::styled(format!("{mark}{indent}{glyph} "), dim),
                    Span::styled(
                        format!("{name}/"),
                        bg(Style::default()
                            .fg(self.theme.header_fg)
                            .add_modifier(Modifier::BOLD)),
                    ),
                    Span::styled("  ", dim),
                    Span::styled(
                        format!("+{adds}"),
                        bg(Style::default().fg(self.theme.add_fg)),
                    ),
                    Span::styled(" ", dim),
                    Span::styled(
                        format!("−{dels}"),
                        bg(Style::default().fg(self.theme.del_fg)),
                    ),
                ])]
            }
            TreeKind::File { file_idx } => {
                let f = &self.files()[*file_idx];
                let name = basename(&f.path).to_string();
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
                        bg(Style::default().fg(self.theme.add_fg)),
                    ));
                    spans.push(Span::styled(" ", dim));
                    spans.push(Span::styled(
                        format!("−{}", f.counts.dels),
                        bg(Style::default().fg(self.theme.del_fg)),
                    ));
                }
                vec![Line::from(spans)]
            }
        }
    }

    /// The flat file list, floating over the foot of the plan pane: where you
    /// are, and how much is left.
    /// Where the file list floats while the diff has focus: the foot of the
    /// plan pane, or nowhere when there are no files. Shared with the hit
    /// test, so a click on the float is known to be one.
    pub fn file_list_area(&self, plan: Rect) -> Option<Rect> {
        let files_len = self.listed_files.len();
        if files_len == 0 {
            return None;
        }
        let h = (files_len as u16 + 2)
            .min(plan.height.saturating_sub(2))
            .max(3);
        Some(Rect {
            x: plan.x,
            y: plan.y + plan.height.saturating_sub(h),
            width: plan.width,
            height: h,
        })
    }

    pub(super) fn draw_file_list(&self, frame: &mut Frame, plan: Rect) {
        let Some(area) = self.file_list_area(plan) else {
            return;
        };
        clear_to_ground(frame, &self.theme, area);
        self.draw_file_list_in(frame, area);
    }

    pub(super) fn draw_file_list_in(&self, frame: &mut Frame, area: Rect) {
        let reviewed = &self.reviewed;
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
                    self.theme.reviewed_fg
                } else {
                    self.theme.context_fg
                });
                let style = if on {
                    base.bg(self.theme.selected_bg).add_modifier(Modifier::BOLD)
                } else {
                    base
                };
                let name = basename(&f.path);
                // No marker glyph: the row the reader is on is the one lit
                // edge to edge, which says it in the one place they are
                // already looking.
                let mut line = Line::from(vec![
                    Span::styled("  ".to_string(), style),
                    Span::styled(name.to_string(), style),
                ]);
                if on {
                    pad_to_width(&mut line, inner_w, self.theme.selected_bg);
                }
                line
            })
            .collect();
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no files)",
                Style::default().fg(self.theme.gutter_fg),
            )));
        }
        let title = match at {
            Some(n) => format!(" file {} of {} ", n + 1, files.len()),
            None => format!(" {} files ", files.len()),
        };
        frame.render_widget(
            Paragraph::new(lines).block(pane(&self.theme, title, true)),
            area,
        );
    }

    /// The right pane while the plan has focus: the whole document's file tree
    /// with the selected group's files lit, so what a group spans is one look
    /// rather than a walk through its hunks.
    ///
    /// It floats over the FOOT of the detail pane at full pane width — the same
    /// shape as the file list at the foot of the plan pane, so one focus reads
    /// like the other. The diff carries on above it as a preview of what
    /// entering the group will show.
    ///
    /// Deliberately not interactive. It is a map; a second cursor in a second
    /// pane is a thing to explain and to get wrong.
    /// Where the group map floats while the plan has focus, or nowhere when the
    /// pane is too short to hold it. Shared with the hit test.
    pub fn group_map_area(&self, detail: Rect) -> Option<Rect> {
        // The group's header block is what the height is capped against, so its
        // full label and description — which the 40-column plan pane truncates —
        // stay readable however many files the group touches.
        let header = self
            .rows
            .iter()
            .take_while(|r| matches!(r.kind, RowKind::GroupHeader | RowKind::Blank))
            .count()
            .min(6) as u16;
        // A pane too short to hold the header block AND a box yields the BOX. A
        // floor that beat the cap would cover the very label the cap exists to
        // protect, which is the one thing the reader cannot do without.
        let cap = detail.height.saturating_sub(header + 2);
        if cap < 3 {
            return None;
        }
        let h = (self.map_rows.len() as u16 + 2).max(3).min(cap);
        Some(Rect {
            x: detail.x,
            y: detail.y + detail.height.saturating_sub(h),
            width: detail.width,
            height: h,
        })
    }

    /// Where the symbol float sits, or nowhere when it will not fit.
    ///
    /// **It never covers the cursor's row.** That row is the question; the
    /// float is the answer, and an answer that hides the question is worse than
    /// no answer. So it takes the larger of the two gaps and stops one row
    /// short of the cursor.
    ///
    /// **Below by preference.** Reading runs downwards, and a float that
    /// appears below the line keeps the eye travelling the way it already was.
    /// It goes above only when above has more room.
    ///
    /// One function for the draw and the hit test, the way `group_map_area` is:
    /// two answers to "where is it" is how a click lands somewhere the float is
    /// not.
    pub fn peek_area(&self, detail: Rect) -> Option<Rect> {
        let peek = self.peek.as_ref()?;
        let inner_h = detail.height.saturating_sub(2) as usize;
        // Where the cursor's row starts and ends, in screen lines from the top
        // of the pane. Walked from `scroll` over the same heights the scroll
        // budget uses, so the float lands where the row actually is.
        let mut top = 0usize;
        for i in self.scroll..peek.row {
            top += self.row_height(i);
            if top >= inner_h {
                return None;
            }
        }
        let bottom = top + self.row_height(peek.row);
        if bottom > inner_h {
            return None;
        }

        // The gaps either side, in rows, with the cursor's own row excluded
        // from both.
        let below = inner_h.saturating_sub(bottom);
        let above = top;
        let (room, downwards) = if below >= above {
            (below, true)
        } else {
            (above, false)
        };
        // A frame plus one line of code is three rows. Less than that and the
        // float would be a box with nothing in it, so it yields entirely —
        // the same "too short, show nothing" rule the group map follows.
        if room < 3 {
            return None;
        }
        let wanted = peek.body.len() + usize::from(peek.more > 0) + 2;
        let h = wanted.clamp(3, room) as u16;
        let y = if downwards {
            detail.y + 1 + bottom as u16
        } else {
            detail.y + 1 + top as u16 - h
        };
        Some(Rect {
            x: detail.x,
            y,
            width: detail.width,
            height: h,
        })
    }

    /// The float: the declaration, with its own line numbers.
    pub(super) fn draw_peek(&self, frame: &mut Frame, detail: Rect) {
        let (Some(peek), Some(area)) = (self.peek.as_ref(), self.peek_area(detail)) else {
            return;
        };
        let inner = area.width.saturating_sub(2) as usize;
        let body_rows = area.height.saturating_sub(2) as usize;
        // What the pane can hold may be less than what was read, and the tally
        // has to count BOTH cuts or it would under-report after a resize.
        let shown = body_rows
            .saturating_sub(usize::from(peek.more > 0))
            .min(peek.body.len());
        let cut = peek.more + (peek.body.len() - shown);

        // The number column is as wide as its widest number, so the code
        // starts in one place.
        let width = peek
            .body
            .iter()
            .take(shown)
            .map(|(n, _)| n.to_string().len())
            .max()
            .unwrap_or(1);
        let mut lines: Vec<Line> = peek
            .body
            .iter()
            .take(shown)
            .map(|(n, pairs)| {
                let mut spans = vec![Span::styled(
                    format!("{n:>width$} ", n = n, width = width),
                    Style::default().fg(self.theme.gutter_fg),
                )];
                spans.extend(
                    slice_pairs(pairs, 0, inner.saturating_sub(width + 1))
                        .into_iter()
                        .map(|(st, t)| Span::styled(t, st)),
                );
                Line::from(spans)
            })
            .collect();
        if cut > 0 {
            lines.push(Line::from(Span::styled(
                format!("{:>width$} … {cut} more lines", "", width = width),
                Style::default().fg(self.theme.gutter_fg),
            )));
        }

        clear_to_ground(frame, &self.theme, area);
        frame.render_widget(
            // Padded here, not in the model: a test asserting WHICH symbol is
            // open reads the title, and it should not have to know how a
            // border draws one. Every other float pads at this same point.
            Paragraph::new(lines).block(pane(&self.theme, format!(" {} ", peek.title), true)),
            area,
        );
    }

    pub(super) fn draw_group_map(&self, frame: &mut Frame, detail: Rect) {
        let Some(area) = self.group_map_area(detail) else {
            return;
        };
        // Read, not recomputed. This walked the whole tree on every frame:
        // an ancestor pass over every row above each live file, and a scan
        // forward per folded directory. It depends on `tree` and `map_files`
        // and nothing else, both of which `rebuild_overviews` already owns.
        let rows = &self.map_rows;
        clear_to_ground(frame, &self.theme, area);
        let inner_h = area.height.saturating_sub(2) as usize;
        let dim = Style::default().fg(self.theme.gutter_fg);
        let guides = guides_for_depths(&rows.iter().map(MapRow::depth).collect::<Vec<_>>());
        let lines: Vec<Line> = rows
            .iter()
            .zip(&guides)
            .map(|(row, guide)| {
                let lead = Span::styled(format!("  {guide}"), dim);
                match row {
                    MapRow::Dir { name, .. } => Line::from(vec![
                        lead,
                        Span::styled(
                            format!("{name}/"),
                            Style::default().fg(self.theme.context_fg),
                        ),
                    ]),
                    // A folded directory keeps the file view's own fold marker,
                    // and says how much it stands for — a row that hid six
                    // files without saying so would read as a directory the
                    // document happens to have nothing in.
                    MapRow::Folded { name, files, .. } => Line::from(vec![
                        lead,
                        Span::styled(format!("▸ {name}/"), dim),
                        Span::styled(format!("  {files} file{}", plural(*files)), dim),
                    ]),
                    MapRow::More { files, .. } => {
                        Line::from(vec![lead, Span::styled(format!("… {files} more"), dim)])
                    }
                    // Every file row IS one the group touches — the rest fold
                    // into a `…` row — so it is always lit, and the dot and
                    // the counts are unconditional.
                    MapRow::File { file_idx, .. } => {
                        let f = &self.files()[*file_idx];
                        let name = basename(&f.path);
                        let style = Style::default()
                            .fg(self.theme.context_fg)
                            .add_modifier(Modifier::BOLD);
                        // The marker sits WITH the name, not out in a column of
                        // its own — a dot at the far left of a deep tree points
                        // at nothing.
                        Line::from(vec![
                            lead,
                            Span::styled(
                                "● ".to_string(),
                                Style::default().fg(self.theme.header_fg),
                            ),
                            Span::styled(name.to_string(), style),
                            Span::styled("  ", style),
                            Span::styled(
                                format!("+{}", f.counts.adds),
                                Style::default().fg(self.theme.add_fg),
                            ),
                            Span::styled(" ", style),
                            Span::styled(
                                format!("−{}", f.counts.dels),
                                Style::default().fg(self.theme.del_fg),
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
            .block(pane(&self.theme, title, true)),
            area,
        );
    }

    /// File indices the selected group touches, via the projection rather than
    /// by re-deriving what belongs to a group here.
    pub(super) fn files_of_selected_group(&self) -> HashSet<usize> {
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
    /// The computation behind the `map_rows` field. Called only from
    /// `rebuild_overviews`, because it reads nothing a frame can change.
    pub(super) fn compute_map_rows(&self) -> Vec<MapRow> {
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
            basename(path).to_string()
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

    /// The publish modal's text: what leaves, what stays, and why — before
    /// anything leaves. The one outward act in this reviewer, so it reads its
    /// whole consequence back before asking (ADR 0029).
    pub(super) fn publish_lines(
        &self,
        plan: &differential_engine::forge::PublishPlan,
    ) -> Vec<Line<'static>> {
        let key = Style::default().fg(self.theme.header_fg);
        let text = Style::default().fg(self.theme.context_fg);
        let dim = Style::default().fg(self.theme.gutter_fg);
        let (comments, replies) = (plan.batch.comments.len(), plan.batch.replies.len());
        let mut lines = vec![Line::from("")];
        let mut what = Vec::new();
        if comments > 0 {
            what.push(format!("{comments} new comment{}", plural(comments)));
        }
        if replies > 0 {
            what.push(format!(
                "{replies} repl{}",
                if replies == 1 { "y" } else { "ies" }
            ));
        }
        lines.push(Line::from(Span::styled(
            format!(
                "  {} go to the pull request as one review",
                what.join(" and ")
            ),
            text,
        )));
        if !plan.excluded.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  {} stay local:", plan.excluded.len()),
                text,
            )));
            // The reason on its own line under the place: side by side
            // they overran the box on any path of ordinary length, and
            // a float clips rather than wraps.
            for ex in &plan.excluded {
                lines.push(Line::from(Span::styled(
                    format!("    {}:{}", ex.file, ex.lines),
                    key,
                )));
                lines.push(Line::from(Span::styled(
                    format!("      {}", ex.reason),
                    dim,
                )));
            }
        }
        lines.push(Line::from(""));
        lines.push(footer_line(&self.theme, &publish_footer()));
        lines
    }

    /// The delete-comment modal's text.
    pub(super) fn delete_comment_lines(
        &self,
        own: &differential_engine::forge::OwnComment,
    ) -> Vec<Line<'static>> {
        let text = Style::default().fg(self.theme.context_fg);
        let at = &own.at;
        vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  delete your comment at {at} on the pull request?"),
                text,
            )),
            Line::from(""),
            footer_line(&self.theme, &delete_comment_footer()),
        ]
    }

    /// One float over the body: cleared to the theme's ground and framed as a
    /// pane with `title`, in the `area` its caller measured. Every modal but
    /// the composer and the two lists draws through here.
    fn float(&self, frame: &mut Frame, area: Rect, title: &str, paragraph: Paragraph) {
        clear_to_ground(frame, &self.theme, area);
        frame.render_widget(
            paragraph.block(pane(&self.theme, title.to_string(), true)),
            area,
        );
    }

    pub(super) fn draw_diff(&self, frame: &mut Frame, area: Rect) {
        let inner_h = area.height.saturating_sub(2) as usize;
        let inner_w = area.width.saturating_sub(2) as usize;
        // Which box is lit. Only one at a time: a screenful of accents is a
        // screenful of nothing, so every other box is muted to the gutter.
        let active = self.current_hunk();
        // What the selection will actually annotate, not the rows the cursor
        // walked over: it stops at a gap, and the highlight has to say so.
        let selection = self.visual.and_then(|_| {
            let rows: Vec<usize> = self.selected_run().iter().map(|(i, _)| *i).collect();
            Some((*rows.iter().min()?, *rows.iter().max()?))
        });
        let note = self.note_cluster();
        let in_note = |i: usize| note.is_some_and(|(lo, hi)| (lo..=hi).contains(&i));
        // Each visible row, where it starts, and the lines it takes. Composed
        // ONCE, here: the content, the border glyphs and the cursor bar are
        // three passes over the same rows, and a wrapped row means a screen
        // line is no longer a row index. Heights come from the lines
        // themselves, so the glyphs cannot land a row away from the text.
        //
        // A row is never cut at its TOP, so one taller than the pane pins
        // there and loses its tail.
        // `placed` carries HEIGHTS, not lines. The composed text moves
        // straight into `lines`; the two passes below only ever asked how tall
        // each row was, and cloning the whole visible pane — every `Line` a
        // vector of `Cow` spans — to answer that was the most expensive thing
        // a repaint did.
        let mut placed: Vec<(usize, u16, usize)> = Vec::new();
        let mut lines: Vec<Line> = Vec::new();
        let mut y = 0usize;
        for i in self.scroll..self.rows.len() {
            if y >= inner_h {
                break;
            }
            let r = &self.rows[i];
            let on = i == self.cursor && self.focus == Focus::Detail && r.kind.selectable();
            // A hunk's pill follows its edge, so the marker and the run
            // below it read as one thing — and which is lit is a cursor
            // question, decided here rather than when the row was built.
            let marker = match (r.border, &r.kind) {
                (Some(b), RowKind::HunkHeader { .. }) if active == Some(b.hunk) => {
                    b.active_style.fg.map_or(Marker::Idle(&r.idle), Marker::Lit)
                }
                (_, RowKind::HunkHeader { .. }) => Marker::Idle(&r.idle),
                (_, RowKind::Finding(..) | RowKind::Thread { .. }) if in_note(i) => Marker::Note,
                _ => Marker::None,
            };
            // How to work this row, on the one row it can be worked from.
            let hint = on.then_some(r.hint.as_ref()).flatten();
            // The cursor's row is inside the run too — it is one end of it —
            // so its code takes the selected tint like the rest, and what
            // keeps it the brighter end is the gutter block and the bar.
            let selected = selection.is_some_and(|(lo, hi)| (lo..=hi).contains(&i));
            let composed: Vec<Line> = compose_row_lines(
                &self.theme,
                &r.content,
                inner_w,
                Paint {
                    cursor: on,
                    selected,
                    marker,
                    hint,
                    wrap: self.wraps(r),
                    hscroll: self.shift(r),
                    // Only the row the float belongs to. The float closes when
                    // the cursor moves, so this is always the cursor's row —
                    // keyed on the row anyway, so the two can never disagree.
                    peek: self.peek.as_ref().filter(|p| p.row == i).map(|p| p.at),
                },
            )
            .into_iter()
            .map(|mut line| {
                if on {
                    // Span backgrounds win over a line style, so this
                    // colours exactly the rows that have no change colour
                    // of their own — on the rest, the brightened gutter
                    // block carries it.
                    line = line.style(Style::default().bg(self.theme.cursor_bg));
                } else if selected {
                    // The same job for a selected row with no colour of its
                    // own: a context line, and the hatched half of a split
                    // row. Where the row IS coloured, `step_band` has already
                    // stepped it — a line style would never have been seen.
                    line = line.style(Style::default().bg(self.theme.selected_bg));
                }
                line
            })
            .collect();
            y += composed.len();
            placed.push((i, (y - composed.len()) as u16, composed.len()));
            lines.extend(composed);
        }
        lines.truncate(inner_h);

        // Scrolled past a file's header, pin it to the top row. It costs a row
        // only while the filename would otherwise be off-screen, which is
        // exactly when a long file stops saying which file it is.
        if let Some(header) = self
            .file_header_above(self.scroll)
            .filter(|&h| h < self.scroll)
            && let Some(first) = lines.first_mut()
        {
            // Its first line only: the pin costs the reader one row by
            // design, wrapped or not.
            *first = compose_row_lines(
                &self.theme,
                &self.rows[header].content,
                inner_w,
                Paint::plain(false),
            )
            .swap_remove(0)
            .style(Style::default().bg(self.theme.sticky_bg));
        }

        let block = pane(
            &self.theme,
            " detail ".to_string(),
            self.focus == Focus::Detail,
        );
        frame.render_widget(Paragraph::new(lines).block(block), area);

        // A hunk's edge shares the pane's left border column rather than
        // sitting a cell inside it: no width lost, and no second vertical line
        // a cell away from the first. Drawn over the block, so it comes after.
        let buf = frame.buffer_mut();
        let on_cursor = |i: usize| i == self.cursor && self.focus == Focus::Detail;
        for &(i, top, height) in &placed {
            let row = &self.rows[i];
            // Every line of the row, not just its first: a hunk's edge is a
            // continuous run down the pane, and a gap in it would read as the
            // hunk ending there.
            for line in 0..height {
                let y = area.y + 1 + top + line as u16;
                if y >= area.y + 1 + inner_h as u16 {
                    break;
                }
                // A control's button takes the same column a hunk's edge
                // would, and lightens with the band it belongs to.
                if let Some(glyph) = row.button {
                    let band = Style::default()
                        .fg(self.theme.hint_fg)
                        .bg(self.theme.hint_bg);
                    let cell = &mut buf[(area.x, y)];
                    // The glyph names the control once. A column of them down
                    // a wrapped row would be a column of buttons that are all
                    // the same button.
                    if line == 0 {
                        cell.set_symbol(glyph);
                    }
                    cell.set_style(if on_cursor(i) {
                        self.theme.lit_band(band)
                    } else {
                        band
                    });
                    continue;
                }
                if let Some(border) = row.border {
                    let cell = &mut buf[(area.x, y)];
                    cell.set_symbol(border.glyph().encode_utf8(&mut [0u8; 4]));
                    cell.set_style(chrome(&self.theme, border, active));
                }
                // A note and the line it annotates, while the cursor is on
                // either: the border column carries the findings colour down
                // both, which is what says they are one thing. Whatever glyph
                // is in that cell — a hunk's edge, or the pane's own border on
                // a note's row — keeps its shape and takes the colour.
                if in_note(i) {
                    buf[(area.x, y)].set_fg(self.theme.finding_fg);
                }
            }
        }

        // The cursor's bar, in the cell just inside the frame. The gutter block
        // says which LINE the cursor is on, but only a diff row has a gutter:
        // on a fold or a boundary the cursor was a faint tint and nothing else.
        // The bar is on every selectable row, so the cursor is one thing to
        // look for rather than two.
        //
        // Not on a hunk HEADER, though. That row is flush to the border, so its
        // pill's leading cell IS this cell — and standing on the header lights
        // that cell already, in the hunk's own accent. Drawing over it would
        // repaint green (reviewed) or muted cyan (foreign) as plain cyan, and
        // say the hunk was neither.
        //
        // Keeps the cell's own background — over a lit gutter it stands on the
        // change colour rather than punching a hole in it.
        //
        // Down EVERY line of a wrapped row: the row is what the cursor is on,
        // and a bar against its first line only would read as the cursor being
        // on that line rather than on the whole of it.
        if self.focus == Focus::Detail
            && let Some((top, height)) = placed
                .iter()
                .find(|(i, _, _)| *i == self.cursor)
                .map(|&(_, top, height)| (top, height))
            && self.rows.get(self.cursor).is_some_and(|r| {
                r.kind.selectable() && !matches!(r.kind, RowKind::HunkHeader { .. })
            })
        {
            for line in 0..height {
                let y = area.y + 1 + top + line as u16;
                if y >= area.y + 1 + inner_h as u16 {
                    break;
                }
                let cell = &mut buf[(area.x + 1, y)];
                cell.set_symbol(CURSOR_BAR);
                cell.set_fg(self.theme.header_fg);
                cell.modifier.insert(Modifier::BOLD);
            }
        }

        // Last, over the content and the border both: the float is an answer
        // laid on top of the pane, not a part of it. Its own area is measured
        // from the model, so drawing it here costs nothing that `placed` knows.
        self.draw_peek(frame, area);
    }

    /// The whole footer, in the order it is drawn: the pills and message, the
    /// keys that fit beside them, and the column those keys start at. One
    /// function for the draw and the hit test, as `centered_x` is for the
    /// composer's footer: a footer laid out twice is how a button drifts off
    /// the key under the pointer.
    ///
    /// A narrow terminal keeps the pills and `? help` and drops the rest: the
    /// keys are the convenience, and the pills are the state of the review.
    pub(super) fn status_row(&self, area: Rect) -> (Vec<Span<'static>>, Vec<Hint>, u16) {
        let mut acts = self.footer_hints();
        let left = self.status_left();
        let width_of = |spans: &[Span]| -> usize {
            spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum()
        };
        let room = usize::from(area.width).saturating_sub(width_of(&left) + 1);
        // Dropped one at a time, from the left: the last of them is `? help`,
        // which is the way to every key that did not fit, so it goes last of
        // all. A footer that dropped the lot at the first column too few
        // emptied itself on an ordinary 100-column terminal.
        while hints_width(&joined(&acts)) > room && acts.len() > 1 {
            acts.remove(0);
        }
        let hints = if hints_width(&joined(&acts)) <= room {
            joined(&acts)
        } else {
            Vec::new()
        };
        let width = hints_width(&hints);
        let x0 = area.x
            + area
                .width
                .saturating_sub(u16::try_from(width + 1).unwrap_or(u16::MAX));
        (left, hints, x0)
    }

    /// Where the footer's keys are, for a click. The pills the draw needs
    /// alongside them are of no interest to a hit test.
    pub fn status_hints(&self, area: Rect) -> (Vec<Hint>, u16) {
        let (_, hints, x0) = self.status_row(area);
        (hints, x0)
    }

    /// The footer's left half: the pills, then the transient message. Its
    /// width is what the keys on the right have to fit beside, so it is built
    /// once per footer and measured there.
    fn status_left(&self) -> Vec<Span<'static>> {
        let total: usize = self.groups().iter().map(|g| g.hunks.len()).sum();
        let done = self.session.reviewed_count().min(total);
        // Open and not yet on the request: what `y` copies and `P` sends. A
        // published note is the forge's now and is counted among its threads.
        let open = self.session.unpublished().count();

        let bar = Style::default().bg(self.theme.status_bg);
        let (ink, fill) = self.theme.pill();
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
        // Being mid-selection is a FACT about the thing in front of the
        // reader, which is exactly what a pill says here — as a group's role
        // and a hunk's class do. It leads the footer, and the count is the
        // point: a selection stops at a context boundary, so it is not always
        // the distance the cursor travelled.
        if self.visual.is_some() {
            let n = self.selected_run().len();
            left.extend(
                pill(
                    vec![(
                        self.theme.header_fg,
                        format!("selecting {n} line{}", plural(n)),
                    )],
                    fill,
                )
                .into_iter()
                .map(|(st, t)| Span::styled(t, st)),
            );
            left.push(Span::styled(" ", bar));
        }
        // Where along the line the pane is standing. Without it a reader who
        // shifted right and then moved to a short file sees an empty pane and
        // nothing that says why — and the way back is a key they would have to
        // go and look for.
        if self.hscroll > 0 {
            left.extend(
                pill(
                    vec![(self.theme.header_fg, format!("+{} cols", self.hscroll))],
                    fill,
                )
                .into_iter()
                .map(|(st, t)| Span::styled(t, st)),
            );
            left.push(Span::styled(" ", bar));
        }
        left.extend(tally(
            total > 0 && done == total,
            self.theme.reviewed_fg,
            format!("{done}/{total} classes reviewed"),
        ));
        left.push(Span::styled(" ", bar));
        // The pill carries the key that opens the list. A count with no way
        // to reach what it counts is a reader asking "and where are they?",
        // and `F` is not a key they can guess from a number.
        left.extend(tally(
            open > 0,
            self.theme.finding_fg,
            format!("{open} finding{}(F)", plural(open)),
        ));
        // The forge's threads are a fact about the request, worn the same way
        // — and only on a review that is of a request, since a range has none.
        if self.has_forge() {
            let threads = self.session.threads().len();
            left.push(Span::styled(" ", bar));
            left.extend(tally(
                threads > 0,
                self.theme.header_fg,
                format!("{threads} thread{}", plural(threads)),
            ));
            // A call is out. The pill IS the state, as `selecting` is: it
            // appears while the answer is awaited and goes when it lands.
            if self.syncing() {
                left.push(Span::styled(" ", bar));
                left.extend(tally(true, self.theme.header_fg, "syncing".to_string()));
            }
        }
        if !self.status.is_empty() {
            left.push(Span::styled(
                format!("  {}", self.status),
                bar.fg(self.theme.context_fg),
            ));
        }
        left
    }

    pub(super) fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let bar = Style::default().bg(self.theme.status_bg);
        // The acts of where the reader is standing, and `? help` behind them
        // (issue 30). A fixed list of ten keys was a wall the reader stopped
        // seeing; three keys that change with the place are three keys they
        // read. The same table feeds `?`, so the two cannot drift.
        let used = |spans: &[Span]| -> usize {
            spans
                .iter()
                .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
                .sum()
        };
        let (left, hints, x0) = self.status_row(area);
        let right: Vec<Span> = footer_line(&self.theme, &hints)
            .spans
            .into_iter()
            .map(|s| Span::styled(s.content, s.style.bg(self.theme.status_bg)))
            .collect();
        let gap = usize::from(x0.saturating_sub(area.x)).saturating_sub(used(&left));
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
/// What drawing knows about a row that building it could not.
///
/// Every field here turns on where the cursor is or what the reader has
/// toggled, and rows are built once and drawn on every key — so none of it can
/// live on the row. It is the argument list `compose_row_lines` already had,
/// named: five of them travel together through three functions, and a sixth
/// pushed the list past what clippy will accept.
#[derive(Clone, Copy)]
pub(super) struct Paint<'a> {
    /// The row the cursor is on.
    pub cursor: bool,
    /// A row inside the open line selection — the cursor's row included, since
    /// it is one end of the run.
    pub selected: bool,
    pub marker: Marker<'a>,
    pub hint: Option<&'a (Style, String)>,
    pub wrap: bool,
    /// How far the row's CONTENT is shifted left, in columns.
    ///
    /// Applied per half, from each half's own left edge, so a split row's two
    /// columns stay comparable. The line-number cell never moves: it is what
    /// the cursor block lands in, and the spec's rule is that the cell keeps
    /// its width and its column.
    ///
    /// A wrapped row ignores it. There is nothing off the edge to reach, and
    /// shifting a row that already shows all of itself is a row with a hole at
    /// the front.
    pub hscroll: usize,
    /// The symbol `z` is showing, as a byte range in the NEW half's drawn text.
    ///
    /// Here rather than on the row for the same reason `cursor` is: which
    /// symbol is lit is a cursor question, and a row that had to be rebuilt to
    /// answer it would rebuild on every press (ADR 0032).
    pub peek: Option<(usize, usize)>,
}

impl Paint<'_> {
    /// A row drawn as it was built: no cursor, no selection, no pill, no hint.
    ///
    /// The pinned file header and the height measurement both want exactly
    /// this, and both used to spell out six arguments to say it.
    pub(super) fn plain(wrap: bool) -> Self {
        Paint {
            cursor: false,
            selected: false,
            marker: Marker::None,
            hint: None,
            wrap,
            hscroll: 0,
            peek: None,
        }
    }
}

/// A row, as the screen lines it takes.
///
/// One line unless `wrap` is on and the content is wider than the pane. A
/// wrapped row is still ONE row: the cursor indexes rows, a finding anchors to
/// a line, and a line that became three selectable rows would let a reader
/// annotate a third of it.
pub(super) fn compose_row_lines(
    theme: &Theme,
    content: &RowContent,
    width: usize,
    paint: Paint<'_>,
) -> Vec<Line<'static>> {
    // Only the header's pill and its hint are read here; the rest travels on
    // into the halves, which is where a colour is chosen.
    let Paint { marker, hint, .. } = paint;
    match content {
        RowContent::Full(line) => vec![line.clone()],
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
                    // The rail only. The prose stays quiet: what the colour
                    // says is which rows belong together, not read this.
                    Marker::Note if !pairs.is_empty() => {
                        pairs[0].0 = pairs[0].0.fg(theme.finding_fg);
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
            let lit;
            let half = match paint.peek {
                Some(at) => {
                    lit = lit_symbol(theme, half, at);
                    &lit
                }
                None => half,
            };
            compose_half_lines(theme, half, width, paint)
                .into_iter()
                .map(Line::from)
                .collect()
        }
        RowContent::Split { old, new } => {
            let (lw, rw) = half_widths(width);
            // Both gutters light: a split row IS one row, and a cursor that
            // showed on one side only read as a cursor on that side's line.
            let mut left = compose_half_lines(theme, old, lw, paint);
            // The NEW half only. The index is keyed by new-side line, so the
            // old half is a different line and lighting the same columns there
            // would point at whatever happens to sit under them.
            let lit;
            let new = match paint.peek {
                Some(at) => {
                    lit = lit_symbol(theme, new, at);
                    &lit
                }
                None => new,
            };
            let mut right = compose_half_lines(theme, new, rw, paint);
            // A row is as tall as its taller half, and the shorter one pads —
            // the rule the `╱` fill already follows across a row, applied down
            // one.
            let h = left.len().max(right.len());
            while left.len() < h {
                left.push(compose_half(theme, &continued(old), lw, paint));
            }
            while right.len() < h {
                right.push(compose_half(theme, &continued(new), rw, paint));
            }
            left.into_iter()
                .zip(right)
                .map(|(mut spans, r)| {
                    spans.push(Span::styled("│", Style::default().fg(theme.gutter_fg)));
                    spans.extend(r);
                    Line::from(spans)
                })
                .collect()
        }
    }
}

/// Repaint one token of a half in the accent, leaving its syntax colours.
///
/// The same splitter word-level diff emphasis uses, so the two compose: a
/// symbol inside a changed word keeps the word's tint and gains the underline.
/// Applied BEFORE `compose_half`, which is where a horizontal shift is taken,
/// so a shifted pane cuts the highlight exactly as it cuts the text.
///
/// **An accent and an underline, not a background.** A background here would
/// have to be told apart from `added_bg`, `deleted_bg` and their word-level
/// twins by `Theme::step_band`, which dispatches on colour VALUES — so a new
/// one would need a palette test per theme to prove it never collides. The
/// accent already exists, and an underline is a shape rather than a colour.
fn lit_symbol(theme: &Theme, half: &Half, at: (usize, usize)) -> Half {
    let (start, end) = at;
    if start >= end {
        return half.clone();
    }
    Half {
        gutter: half.gutter.clone(),
        pairs: split_pairs_at_ranges(
            &half.pairs,
            vec![(start, end)],
            Style::default()
                .fg(theme.header_fg)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ),
        fill: half.fill,
    }
}

/// The two column widths a split row lays out in, either side of the `│`.
///
/// One copy, because the overflow a horizontal shift is bounded by has to be
/// measured against the width the content is actually drawn at. Two copies of
/// this arithmetic would let the pane shift past its own longest line, or stop
/// short of it.
pub(super) fn half_widths(width: usize) -> (usize, usize) {
    let lw = width.saturating_sub(1) / 2;
    (lw, width.saturating_sub(1).saturating_sub(lw))
}

/// How many columns of a row's content fall off the right edge at `width`.
///
/// Zero for a row that fits. What bounds the horizontal shift: past the widest
/// row's overflow there is nothing left to reveal.
pub(super) fn overflow(content: &RowContent, width: usize) -> usize {
    let over = |h: &Half, w: usize| {
        let rest = w.saturating_sub(UnicodeWidthStr::width(h.gutter.text.as_str()));
        h.pairs
            .iter()
            .map(|(_, t)| t.width())
            .sum::<usize>()
            .saturating_sub(rest)
    };
    match content {
        // A banner is built to the pane it is drawn in.
        RowContent::Full(_) => 0,
        RowContent::Unified(half) => over(half, width),
        RowContent::Split { old, new } => {
            let (lw, rw) = half_widths(width);
            over(old, lw).max(over(new, rw))
        }
    }
}

/// A half with nothing left to say: the padding under a side that ran out of
/// lines before the other did. It keeps the row's fill, so an absent line goes
/// on hatching and a change goes on carrying its colour.
pub(super) fn continued(half: &Half) -> Half {
    Half {
        gutter: blank_gutter(&half.gutter),
        pairs: Vec::new(),
        fill: half.fill,
    }
}

/// The line-number cell on a wrapped row's later lines.
///
/// Blank, and the SAME WIDTH: a continuation has no number of its own, and the
/// cursor block has to land in one column whichever line of the row it is on.
pub(super) fn blank_gutter(gutter: &Gutter) -> Gutter {
    Gutter {
        text: " ".repeat(UnicodeWidthStr::width(gutter.text.as_str())),
        style: gutter.style,
        cursor: gutter.cursor,
    }
}

/// One side of a row, as the screen lines it takes.
pub(super) fn compose_half_lines(
    theme: &Theme,
    half: &Half,
    width: usize,
    paint: Paint<'_>,
) -> Vec<Vec<Span<'static>>> {
    let rest = width.saturating_sub(UnicodeWidthStr::width(half.gutter.text.as_str()));
    if !paint.wrap {
        return vec![compose_half(theme, half, width, paint)];
    }
    let blank = blank_gutter(&half.gutter);
    wrap_indented(&half.pairs, rest)
        .into_iter()
        .enumerate()
        .map(|(i, pairs)| {
            let half = Half {
                gutter: if i == 0 {
                    half.gutter.clone()
                } else {
                    blank.clone()
                },
                pairs,
                fill: half.fill,
            };
            compose_half(theme, &half, width, paint)
        })
        .collect()
}

/// A row's content, as the lines it wraps to — each carrying the leading
/// indent and rail of the first.
///
/// A finding is a quoted panel and the rail IS the panel: a note whose second
/// line began at the pane edge would leave the quote hanging open. A group
/// description is indented, and a continuation flush against the border would
/// not read as part of it. Code is the same fact once more — a statement
/// picked up under its own indentation reads as the statement continuing.
pub(super) fn wrap_indented(pairs: &[(Style, String)], width: usize) -> Vec<Vec<(Style, String)>> {
    let plain: String = pairs.iter().map(|(_, t)| t.as_str()).collect();
    let head = plain.find(|c| c != ' ' && c != RAIL).unwrap_or(0);
    let prefix = slice_pairs(pairs, 0, head);
    let indent: usize = prefix.iter().map(|(_, t)| t.width()).sum();
    if head == 0 || indent >= width {
        return wrap_pairs(pairs, width);
    }
    let mut lines = wrap_pairs(&slice_pairs(pairs, head, plain.len()), width - indent);
    for line in lines.iter_mut() {
        let mut out = prefix.clone();
        out.append(line);
        *line = out;
    }
    lines
}

/// What colour a hunk's box and band take right now.
///
/// Deliberately not a flag on the row: the cursor moves without rebuilding
/// rows, so "is this the active hunk" cannot be decided when the row is built.
/// The row carries the colour it WOULD take, and drawing chooses.
pub(super) fn chrome(theme: &Theme, border: Border, active: Option<usize>) -> Style {
    if active == Some(border.hunk) {
        border.active_style
    } else {
        Style::default().fg(theme.gutter_fg)
    }
}

/// One side of a diff row at a known column width: the gutter, the content,
/// and padding out to the edge in whatever the row is filled with.
pub(super) fn compose_half(
    theme: &Theme,
    half: &Half,
    width: usize,
    paint: Paint<'_>,
) -> Vec<Span<'static>> {
    let gutter = half.gutter.text.clone();
    let used = UnicodeWidthStr::width(gutter.as_str());
    let rest = width.saturating_sub(used);
    // The cursor IS the line-number block, brightened. There is no marker glyph
    // to make room for, so the cell never changes width and the pane never
    // shifts sideways as the cursor moves.
    //
    // A selected row leaves this cell alone. The gutter column belongs to the
    // cursor, and it is what keeps the cursor the brighter end of a run whose
    // code now carries a colour of its own.
    let style = if paint.cursor {
        half.gutter.cursor
    } else {
        half.gutter.style
    };

    let mut spans = Vec::new();
    if !gutter.is_empty() {
        spans.push(Span::styled(gutter, style));
    }
    // The shift, applied after the gutter and before anything is measured
    // against the pane: the line-number cell keeps its column, and the cut
    // tail still gets its ellipsis from the width that is left.
    let shifted;
    let half = if paint.hscroll > 0 && !paint.wrap {
        shifted = Half {
            gutter: half.gutter.clone(),
            pairs: drop_columns(&half.pairs, paint.hscroll),
            fill: half.fill,
        };
        &shifted
    } else {
        half
    };
    // Two re-inks, both keyed by colour, both leaving syntax alone.
    //
    // A boundary band carries its own colour the whole way across, so the row
    // tint that marks a lit row everywhere else never showed through it.
    //
    // A cursor's row and a selected row are the same problem: a changed line
    // paints every span with `added_bg`/`deleted_bg`, and a span background
    // beats the line style both used to be drawn with. So the row was lit and
    // said nothing where it mattered — and on a split row it said it on the
    // hatched half alone, which read as a cursor on the side with no line.
    // `step_band` moves the change colour itself, the cursor one rung further
    // than a selection.
    let ink = |st: Style| {
        let st = if paint.cursor { theme.lit_band(st) } else { st };
        if paint.cursor || paint.selected {
            theme.step_band(st, paint.cursor)
        } else {
            st
        }
    };
    let pairs: Vec<(Style, String)> = if paint.cursor || paint.selected {
        half.pairs
            .iter()
            .map(|(st, t)| (ink(*st), t.clone()))
            .collect()
    } else {
        half.pairs.clone()
    };
    match half.fill {
        // The padding out to the pane edge goes through the same ink: a
        // selected line whose colour stopped at its last character would read
        // as a shorter line, not a selected one.
        Fill::Bg(bg) => spans.extend(truncate_or_pad_spans(&pairs, rest, ink(bg))),
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
                    Style::default().fg(theme.hatch_fg),
                ));
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
pub(super) fn tree_guides(tree: &[TreeEntry]) -> Vec<String> {
    guides_for_depths(&tree.iter().map(|e| e.depth).collect::<Vec<_>>())
}

/// The same connectors from depths alone, so a list that is not `TreeEntry`
/// rows — the group map's folded view — draws the identical guides.
pub(super) fn guides_for_depths(depths: &[usize]) -> Vec<String> {
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
pub(super) const CURSOR_BAR: &str = "▌";

/// The lit cell at the head of the hunk pill the cursor is in.
pub(super) const PILL_BAR: &str = "▌";

/// What a hunk's header shows right now.
///
/// Decided at draw time, not when the row is built: whether the cursor is in a
/// hunk changes without rebuilding rows, and the header is the one row whose
/// CONTENT turns on it — idle, it is hatch and nothing else.
#[derive(Clone, Copy)]
pub(super) enum Marker<'a> {
    /// Not a hunk header. Draw the row as it was built.
    None,
    /// A hunk header the cursor is not in: the band, carrying only the marks
    /// the row says survive it.
    Idle(&'a [(Style, String)]),
    /// A hunk header the cursor is in: the pill, its leading cell lit.
    Lit(Color),
    /// A note the cursor is standing in, or beside: its rail takes the
    /// findings colour, so the note and the line it is about read as one
    /// thing rather than as two rows that happen to be adjacent.
    Note,
}

/// A pane's frame: always the muted border, with the TITLE carrying focus.
///
/// A lit border draws a box around half the screen to say a thing about the
/// cursor, which is the smallest thing on it — and it competed with the hunk
/// edge, the one border in this view that means something. The title is where
/// a reader looks to know which pane they are in anyway.
/// Clear a float's area and repaint the theme's ground under it.
///
/// `Clear` resets cells to the TERMINAL's default, which is not the theme's
/// background — so a float over a light palette punched a dark hole in it.
/// Every float goes through here rather than calling `Clear` directly.
pub(super) fn clear_to_ground(frame: &mut Frame, theme: &Theme, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(theme.ground()), area);
}

/// The frame every pane and every box wears: one cell of border each side.
/// `pane` paints it; `pane_inner` reads it. Nothing else names the border,
/// so a change to the frame changes both.
fn frame() -> Block<'static> {
    Block::default().borders(Borders::ALL)
}

/// Rows the frame takes from a box's height: its top and bottom border. A box
/// sized "around `n` lines" is `n + FRAME_ROWS` tall; a unit test holds this
/// to what `frame` actually takes.
pub const FRAME_ROWS: u16 = 2;

/// The content `frame` leaves inside `area`. The rows a click is mapped onto
/// and the footer row are read from here, so a hit test cannot disagree with
/// the draw about where the border is.
pub fn pane_inner(area: Rect) -> Rect {
    frame().inner(area)
}

pub(super) fn pane(theme: &Theme, title: String, focused: bool) -> Block<'static> {
    let ink = if focused {
        theme.header_fg
    } else {
        theme.gutter_fg
    };
    frame()
        .border_style(Style::default().fg(theme.gutter_fg))
        .title(Span::styled(
            title,
            Style::default().fg(ink).add_modifier(Modifier::BOLD),
        ))
}

/// Rows `lines` take when word-wrapped inside `inner` columns, one at least
/// per line. The same wrap the paragraphs draw with, so a box sized by it
/// holds its text.
fn wrapped_rows<'a>(lines: impl Iterator<Item = &'a str>, inner: usize) -> usize {
    let inner = inner.max(1);
    lines.map(|l| textwrap::wrap(l, inner).len().max(1)).sum()
}

/// The composer's box: three fifths of the body wide, and as tall as its
/// text — the frame, the footer and a spare row on top of the lines — up to
/// the body, beyond which the text area scrolls. A float over the diff, not a
/// strip pinned to the bottom: a finding is about the lines you can still see
/// around it. Shared with the hit test.
pub fn composer_area(body: Rect, textarea: &TextArea<'_>) -> Rect {
    let width = body.width * 3 / 5;
    // Rows as wrapped, not lines as typed; the text area scrolls beyond the
    // body anyway.
    let rows = wrapped_rows(
        textarea.lines().iter().map(String::as_str),
        usize::from(width.saturating_sub(FRAME_ROWS)),
    );
    // The frame, the footer and the spare row: that is the four.
    let wanted = u16::try_from(rows)
        .unwrap_or(u16::MAX)
        .saturating_add(FRAME_ROWS + 2);
    let height = wanted.clamp(10, body.height.max(10));
    centered_rect(body, width, height)
}

/// A box centred on the body around `lines` of text, `width` wide at most.
/// Clamped to the body, in which case the last lines are not drawn; see
/// `footer_fits`.
fn box_around(body: Rect, width: u16, lines: usize) -> Rect {
    let height = u16::try_from(lines)
        .unwrap_or(u16::MAX)
        .saturating_add(FRAME_ROWS);
    centered_rect(body, width, height)
}

/// Whether a box `box_around` sized for `lines` was tall enough to hold them
/// all — so its last line, the footer, is on screen to be clicked.
pub(super) fn footer_fits(area: Rect, lines: usize) -> bool {
    usize::from(area.height) == lines + usize::from(FRAME_ROWS)
}

/// The publish modal's box, around `lines` of text.
pub fn publish_area(body: Rect, lines: usize) -> Rect {
    box_around(body, body.width.saturating_sub(6).min(90), lines)
}

/// The delete-comment modal's box, around `lines` of text.
pub fn delete_comment_area(body: Rect, lines: usize) -> Rect {
    box_around(body, body.width.saturating_sub(6).min(80), lines)
}

/// A box's footer: the last content row `frame` leaves inside it.
pub fn footer_row(area: Rect) -> Rect {
    let inner = pane_inner(area);
    Rect {
        x: inner.x,
        y: inner.bottom().saturating_sub(1),
        width: inner.width,
        height: 1,
    }
}

/// Where a line `width` columns wide starts when centred in `row` — the one
/// arithmetic for the composer's footer, drawn and hit-tested, so the two
/// cannot round differently.
pub fn centered_x(row: Rect, width: usize) -> u16 {
    let slack = usize::from(row.width).saturating_sub(width);
    row.x + u16::try_from(slack / 2).unwrap_or(0)
}

/// The composer's keys. The newline hint reads and does nothing on a click:
/// a caret is where a newline goes, and a click has none.
pub fn composer_footer() -> Vec<Hint> {
    vec![
        Hint::button("  enter ", "save", vec![Hint::press(KeyCode::Enter)]),
        Hint::note("  │  ", Ink::Dim),
        Hint {
            pieces: vec![
                ("shift+enter ".into(), Ink::Key),
                ("or".into(), Ink::Text),
                (" \\↵ ".into(), Ink::Key),
                ("newline".into(), Ink::Text),
            ],
            presses: Vec::new(),
        },
        Hint::note("  │  ", Ink::Dim),
        Hint::button("esc ", "cancel", vec![Hint::press(KeyCode::Esc)]),
    ]
}

/// While `D` waits for its answer, the findings modal's footer is the
/// question, with `y` and `n` the two things a click can say. Its keys are
/// the table's (`help.rs`), so only this one state lives here.
pub fn findings_question(local: usize, kept: usize) -> Vec<Hint> {
    let press = |c: char| vec![Hint::press(KeyCode::Char(c))];
    let question = match (local, kept) {
        (1, 0) => "  delete this note?  ".to_string(),
        (n, 0) => format!("  delete all {n} notes?  "),
        (1, k) => format!("  delete this note? ({k} on the request stay)  "),
        (n, k) => format!("  delete all {n} local notes? ({k} on the request stay)  "),
    };
    vec![
        Hint::note(&question, Ink::Warn),
        Hint {
            pieces: vec![("y".into(), Ink::Warn)],
            presses: press('y'),
        },
        Hint::note(" / ", Ink::Warn),
        Hint {
            pieces: vec![("n".into(), Ink::Warn)],
            presses: press('n'),
        },
    ]
}

/// A `y`-only question's footer: `y` does the thing, and a click on the
/// clause about every other key is one of them.
fn yes_or_keep_footer(does: &str, keeps: &str) -> Vec<Hint> {
    vec![
        Hint {
            pieces: vec![("  y".into(), Ink::Key), (format!(" {does}"), Ink::Dim)],
            presses: vec![Hint::press(KeyCode::Char('y'))],
        },
        Hint {
            pieces: vec![(format!("  ·  any other key {keeps}"), Ink::Dim)],
            presses: vec![Hint::press(KeyCode::Esc)],
        },
    ]
}

pub fn publish_footer() -> Vec<Hint> {
    yes_or_keep_footer("publishes", "keeps them local")
}

pub fn delete_comment_footer() -> Vec<Hint> {
    yes_or_keep_footer("deletes it there and here", "keeps it")
}

/// A footer as drawn: each piece in the palette's ink for it.
pub(super) fn footer_line(theme: &Theme, hints: &[Hint]) -> Line<'static> {
    let ink = |ink: Ink| match ink {
        Ink::Key => Style::default().fg(theme.header_fg),
        Ink::Text => Style::default().fg(theme.context_fg),
        Ink::Dim => Style::default().fg(theme.gutter_fg),
        Ink::Warn => Style::default()
            .fg(theme.finding_fg)
            .add_modifier(Modifier::BOLD),
    };
    Line::from(
        hints
            .iter()
            .flat_map(|h| h.pieces.iter())
            .map(|(text, i)| Span::styled(text.clone(), ink(*i)))
            .collect::<Vec<_>>(),
    )
}

/// The file-list modal's box: as tall as its entries and as wide as its
/// widest path, centred on the body. One function for the draw and the hit
/// test, so a click is judged against the box that was drawn.
pub fn file_list_modal_area(body: Rect, entries: &[FileListEntry]) -> Rect {
    let height = (entries.len() + 2).min(body.height as usize) as u16;
    let (_, _, lead) = counts_columns(entries);
    let widest = entries
        .iter()
        .map(|e| UnicodeWidthStr::width(e.path.as_str()))
        .max()
        .unwrap_or(0);
    // The box fits its content, exactly as its height already does — 70
    // columns is a floor, not the size. A fixed width cut deep paths against
    // the border and took the file NAME with them, which is the one part of
    // a path worth reading.
    let width = (lead + widest + 2).max(70).min(body.width as usize) as u16;
    centered_rect(body, width, height)
}

/// The findings modal's box: the entries, their section rules, a title row
/// and the key footer, centred on the body. Shared with the hit test.
pub fn findings_modal_area(body: Rect, entries: usize, rules: usize) -> Rect {
    let height = (entries + rules + 4).min(body.height as usize) as u16;
    // Wide enough for the whole key footer: six keys is 83 columns, and a
    // list whose footer is cut is a list with keys nobody can read.
    centered_rect(body, 86, height)
}

pub(super) fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

pub(super) fn help_lines(theme: &Theme, sections: &[HelpSection]) -> Vec<Line<'static>> {
    // The keys of where the reader is standing, and under a rule the keys
    // that mean the same thing anywhere (issue 30). A list of everything is
    // a list nobody reads to the end, and the reader who presses `?` is
    // asking about the place they are in.
    let key = Style::default().fg(theme.header_fg);
    let text = Style::default().fg(theme.context_fg);
    let dim = Style::default().fg(theme.gutter_fg);

    let row = |k: &str, what: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<13}"), key),
            Span::styled(what.to_string(), text),
        ])
    };
    // No title inside the box: the border already carries one. The section
    // titles are not that — they say WHERE each run of keys applies, which is
    // the whole answer this modal gives.
    let mut lines = vec![Line::from("")];
    for (i, section) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", section.title),
            dim.add_modifier(Modifier::ITALIC),
        )));
        lines.extend(section.acts.iter().map(|a| row(&a.key, &a.help)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  press any key to close", dim)));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FRAME_ROWS` is a number about `frame`, and this is what ties them.
    #[test]
    fn the_frame_takes_frame_rows() {
        let area = Rect::new(0, 0, 20, 10);
        let inner = pane_inner(area);
        assert_eq!(area.height - inner.height, FRAME_ROWS);
        assert_eq!(area.width - inner.width, FRAME_ROWS);
        assert_eq!(footer_row(area).y, inner.bottom() - 1);
    }
}
