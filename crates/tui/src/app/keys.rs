//! The whole input surface: one `handle_key`, one `handle_paste`.
//!
//! A plain method on the model returning effects, so a test drives the
//! reviewer without a terminal. Modal arms return early; the normal-mode arm
//! is the tail.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::rows::RowKind;

use super::text::{basename, file_list_rows, findings_rows, step_list};
use super::*;

/// Columns one press of `h`/`l` moves the diff pane.
///
/// One indent step. A single column is what vim moves and it is eighty
/// presses to reach column eighty; a half pane overshoots the word the reader
/// was following.
const SHIFT_STEP: isize = 8;

impl App {
    /// Text pasted into the terminal.
    ///
    /// Bracketed paste is enabled so a multi-line paste arrives as ONE event
    /// rather than as a run of keys that would each drive a normal-mode
    /// action. The event was then dropped, which meant pasting into the
    /// finding composer did nothing at all.
    ///
    /// Only the composer takes it: in normal mode there is no text field for
    /// it to land in, and a paste there is a mis-aimed one.
    pub fn handle_paste(&mut self, text: &str) {
        if let Mode::Editing { editor, .. } = &mut self.mode {
            editor.insert_str(text);
        }
    }

    /// Key handling. Returns effects for the loop to execute.
    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Effect> {
        // One latch, taken before anything reads a key. It used to be taken
        // inside the normal-mode block, which a modal's early return never
        // reaches — so `dd` could only ever mean one thing in one place.
        let pending_d = std::mem::take(&mut self.pending_d);
        // The footer's message answers "what did that key just do", so the
        // next key is exactly when the answer stops being wanted. Cleared
        // HERE, before any handler runs: 35 places write this field and one
        // used to clear it, which made every one-off message permanent.
        self.status.clear();
        match &mut self.mode {
            Mode::Help => {
                self.mode = Mode::Normal;
                return Vec::new();
            }
            Mode::FileList {
                entries,
                selected,
                scroll,
            } => {
                let rows = file_list_rows(entries.len(), self.viewport.body_rows);
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        step_list(selected, scroll, entries.len(), rows, true);
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        step_list(selected, scroll, entries.len(), rows, false);
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
            Mode::Findings {
                entries,
                selected,
                scroll,
                confirming,
            } => {
                // Asking to delete everything: the next key answers, and only
                // `y` means yes. Anything else is a slip, and a slip must not
                // be the thing that empties the store.
                if *confirming {
                    *confirming = false;
                    // A bare `y`, like every other single-character key here.
                    // Some terminals report ctrl-y as `Char('y')` with a
                    // modifier, and the one irreversible action in this
                    // reviewer should not answer to a chord nobody aimed.
                    if (key.code, key.modifiers) == (KeyCode::Char('y'), KeyModifiers::NONE) {
                        self.clear_findings();
                    } else {
                        self.status = "nothing deleted".into();
                    }
                    return Vec::new();
                }
                let ruled = entries.iter().any(|e| e.orphaned);
                let rows = findings_rows(entries.len(), ruled, self.viewport.body_rows);
                match (key.code, key.modifiers) {
                    (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
                        step_list(selected, scroll, entries.len(), rows, true);
                    }
                    (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
                        step_list(selected, scroll, entries.len(), rows, false);
                    }
                    (KeyCode::Char('D'), _) => *confirming = true,
                    (KeyCode::Char('d'), KeyModifiers::NONE) => {
                        if pending_d {
                            let id = entries[*selected].id.clone();
                            self.delete_finding(&id);
                        } else {
                            self.pending_d = true;
                        }
                    }
                    (KeyCode::Enter, _) => {
                        let id = entries[*selected].id.clone();
                        let orphaned = entries[*selected].orphaned;
                        // Assign the mode first: it is what drops the borrow
                        // this arm holds on it.
                        self.mode = Mode::Normal;
                        if orphaned {
                            self.status = "that finding has no line any more".into();
                        } else if !self.jump_to_finding(&id) {
                            self.status = "could not reach that finding".into();
                        }
                    }
                    (KeyCode::Esc, _) | (KeyCode::Char('F'), _) | (KeyCode::Char('q'), _) => {
                        self.mode = Mode::Normal;
                    }
                    _ => {}
                }
                return Vec::new();
            }
            Mode::Editing {
                hunk,
                lines,
                rewriting,
                reply_to,
                editor: textarea,
            } => {
                let (hunk, lines, rewriting, reply_to) =
                    (*hunk, lines.clone(), rewriting.clone(), reply_to.clone());
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) => {
                        self.mode = Mode::Normal;
                        self.status = "finding discarded".into();
                        return Vec::new();
                    }
                    // `enter` saves. A finding is usually one line, and the key
                    // that ends a line is the key a reader reaches for to be
                    // done with it. `ctrl-s` still saves too: it costs one arm,
                    // and it is what the box said for two releases.
                    //
                    // A newline is `shift+enter` where the terminal reports it,
                    // and a trailing `\` before `enter` where it does not —
                    // most terminals send plain `enter` for both unless the
                    // kitty keyboard protocol is on, which this reviewer
                    // deliberately does not ask for.
                    (KeyCode::Enter, m) if m.contains(KeyModifiers::SHIFT) => {
                        textarea.insert_newline();
                        return Vec::new();
                    }
                    (KeyCode::Enter, _)
                        // The character before the CURSOR, not the end of the
                        // line: `delete_char` takes what the cursor sits after,
                        // so a `\` at the end of a line the reader had gone
                        // back to edit would have deleted something else.
                        if {
                            let (row, col) = textarea.cursor();
                            textarea
                                .lines()
                                .get(row)
                                .and_then(|l| col.checked_sub(1).and_then(|i| l.chars().nth(i)))
                                == Some('\\')
                        } =>
                    {
                        textarea.delete_char();
                        textarea.insert_newline();
                        return Vec::new();
                    }
                    (KeyCode::Enter, _) | (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
                        let body = textarea.lines().join("\n").trim().to_string();
                        self.mode = Mode::Normal;
                        match (rewriting, reply_to, body.is_empty()) {
                            // Emptying the box does NOT delete the note. That
                            // is `dd`, which is a deliberate press; a note lost
                            // to a stray `ctrl-u` and an `enter` is not.
                            (Some(_), _, true) => self.status = "finding left as it was".into(),
                            (Some(id), _, false) => self.rewrite_finding(&id, body),
                            (None, _, true) => self.status = "empty finding discarded".into(),
                            (None, Some(thread), false) => self.add_reply(&thread, body),
                            (None, None, false) => self.add_finding(hunk, lines, body),
                        }
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
                self.cursor = self.half_page(self.cursor, 1);
                self.cursor = self.next_selectable(self.cursor, -1).unwrap_or(self.cursor);
                self.follow_cursor();
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.cursor = self.half_page(self.cursor, -1);
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
            // One key for "show me what is being withheld", acting on the pane
            // it is pressed in. Reading the diff that is a context boundary or
            // a folded remainder; reading the file tree it is a directory.
            //
            // The pane matters: `self.cursor` is a DIFF row wherever the focus
            // is, so without it a press in the tree opened whatever the diff's
            // cursor happened to be parked on.
            (KeyCode::Char('z'), _)
                if self.focus == Focus::Detail
                    && matches!(
                        self.rows.get(self.cursor).map(|r| &r.kind),
                        Some(RowKind::ContextEdge { .. })
                    ) =>
            {
                self.expand_at_cursor();
            }
            (KeyCode::Char('z'), _)
                if self.focus == Focus::Groups && self.view_mode == ViewMode::Files =>
            {
                self.toggle_dir();
            }
            (KeyCode::Char('z'), _) => self.toggle_group_fold(),
            (KeyCode::Char('n'), KeyModifiers::NONE) => self.jump_hunk(1),
            (KeyCode::Char('N'), _) => self.jump_hunk(-1),
            (KeyCode::Char('s'), KeyModifiers::NONE) => self.toggle_split(),
            (KeyCode::Char('w'), KeyModifiers::NONE) => self.toggle_wrap(),
            // Sideways, in the pane you are in. A line wider than its column
            // is cut in either layout and twice as often in split, where the
            // column is half a pane; `w` is the other answer and the two are
            // exclusive, which `shift_pane` says out loud.
            //
            // Eight columns is one indent step, so a press moves a distance
            // worth pressing a key for. `0` is the way back, in one.
            (KeyCode::Char('l'), KeyModifiers::NONE) | (KeyCode::Right, _)
                if self.focus == Focus::Detail =>
            {
                self.shift_pane(Some(SHIFT_STEP));
            }
            (KeyCode::Char('h'), KeyModifiers::NONE) | (KeyCode::Left, _)
                if self.focus == Focus::Detail =>
            {
                self.shift_pane(Some(-SHIFT_STEP));
            }
            (KeyCode::Char('0'), KeyModifiers::NONE) if self.focus == Focus::Detail => {
                self.shift_pane(None);
            }
            // One key for files, acting on the pane it is pressed in. In the
            // left pane that is which list of files you are reading — the
            // plan or the tree; in the diff pane it is which file you want to
            // be looking at. `v` used to switch the left pane from either
            // side, which meant a key in one pane silently rearranged the
            // other.
            // Every finding at once, from either pane. It is a fact about the
            // review rather than about a pane, unlike `f`.
            (KeyCode::Char('F'), _) => self.open_findings(),
            (KeyCode::Char('f'), KeyModifiers::NONE) => match self.focus {
                Focus::Groups => self.toggle_file_view(),
                Focus::Detail => self.open_file_list(),
            },
            (KeyCode::Char(' '), _) => self.toggle_reviewed(),
            // A selection, so a finding can be about the lines it is about.
            // One field, not a mode: `j`/`k` keep moving the cursor and the
            // selection is the span between the two ends, which is what makes
            // `V` cost nothing to explain.
            // A toggle. `v` is how a reader gets into a selection, so it is
            // the key their hand is on to get out of one — `esc` works too,
            // and so does `c`, which leaves by using it.
            // Neither end writes a message. The footer's pill IS the state:
            // it appears while a selection is open and goes when it closes,
            // where a passing message described a MODE in the same grey slot
            // that "finding saved" uses for something already over.
            (KeyCode::Char('v'), KeyModifiers::NONE) if self.visual.is_some() => {
                self.visual = None;
            }
            (KeyCode::Char('v'), KeyModifiers::NONE) => {
                if self.rows.get(self.cursor).is_some_and(|r| r.line.is_some()) {
                    self.visual = Some(self.cursor);
                } else {
                    // A refusal, not a mode: nothing happened, so nothing on
                    // screen says why unless the footer does.
                    self.status = "move onto a line first".into();
                }
            }
            (KeyCode::Esc, _) if self.visual.is_some() => {
                self.visual = None;
            }
            // On a forge thread, `c` answers it: the composer opens as a reply,
            // and what it saves is a finding carrying the thread's id until a
            // publish sends it (ADR 0029).
            (KeyCode::Char('c'), KeyModifiers::NONE) if self.thread_at_cursor().is_some() => {
                let t = self.thread_at_cursor().expect("guarded");
                let (id, author, path) = (
                    t.id.clone(),
                    t.root().map(|c| c.author.clone()).unwrap_or_default(),
                    t.path.clone(),
                );
                let hunk = self.current_hunk().unwrap_or(0);
                let mut ta = TextArea::default();
                ta.set_block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(self.theme.header_fg))
                        .title(format!(" {} · reply to {author} ", basename(&path))),
                );
                self.visual = None;
                self.mode = Mode::Editing {
                    hunk,
                    lines: None,
                    rewriting: None,
                    reply_to: Some(id),
                    editor: Box::new(ta),
                };
            }
            (KeyCode::Char('c'), KeyModifiers::NONE) => {
                if let Some(h) = self.current_hunk() {
                    // A line already carrying a note opens THAT note. Two
                    // notes on one line would each be half the story, and
                    // there was no way to correct a typo but delete and
                    // retype. A SELECTION is the exception: picking a run of
                    // lines is asking for a note about the run.
                    let existing = self
                        .visual
                        .is_none()
                        .then(|| self.finding_at_cursor())
                        .flatten()
                        .map(|f| (f.id.clone(), f.body.clone(), f.anchor.line_span()));
                    let lines = self.selected_lines();
                    // Name what is being annotated: a note whose subject you
                    // cannot see is a note you have to trust yourself to have
                    // written carefully.
                    let hunk = &self.session.doc().hunks[h];
                    let file = basename(&hunk.file);
                    let at = match (&existing, &lines) {
                        // A note's own anchor, which may not be the row the
                        // cursor is on — it can have re-anchored to the hunk.
                        (Some((_, _, span)), _) => format!("L{span}"),
                        (None, Some(l)) if l.end > l.start => format!("L{}-{}", l.start, l.end),
                        (None, Some(l)) => format!("L{}", l.start),
                        // No line under the cursor — a hunk header, a fold.
                        // The finding anchors the hunk, so the title says so.
                        (None, None) if hunk.new_count > 1 => format!(
                            "L{}-{}",
                            hunk.new_start,
                            hunk.new_start + hunk.new_count - 1
                        ),
                        (None, None) => format!("L{}", hunk.new_start),
                    };
                    let mut ta = match &existing {
                        Some((_, body, _)) => {
                            TextArea::new(body.lines().map(str::to_string).collect::<Vec<_>>())
                        }
                        None => TextArea::default(),
                    };
                    ta.move_cursor(tui_textarea::CursorMove::End);
                    ta.set_block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(self.theme.header_fg))
                            .title(format!(" {file} · {at} ")),
                    );
                    self.visual = None;
                    self.mode = Mode::Editing {
                        hunk: h,
                        lines,
                        rewriting: existing.map(|(id, _, _)| id),
                        reply_to: None,
                        editor: Box::new(ta),
                    };
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
            // The forge's threads (ADR 0029): resolve the one under the cursor,
            // or fetch them all again. Both go out on a worker thread and
            // land through `poll_forge`.
            (KeyCode::Char('x'), KeyModifiers::NONE) => self.toggle_thread_resolved(),
            (KeyCode::Char('R'), _) => self.start_fetch(),
            _ => {}
        }
        Vec::new()
    }
}
