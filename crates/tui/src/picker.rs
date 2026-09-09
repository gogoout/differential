//! The source picker behind bare `dfr review`.
//!
//! You tick "include uncommitted changes" and pick a BASE commit; the review
//! runs from that commit to either the worktree (ticked) or HEAD. A leading
//! bar marks the rows inside the selected range, so what is covered is
//! visible while choosing.

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, MouseButton, MouseEventKind};
use differential_engine::gitio::Repo;
use differential_engine::ports::{CommitHistory, CommitSummary};
use differential_engine::worktree::is_clean;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::theme::Theme;

/// What the user picked: a base commit, plus whether uncommitted work is in.
pub struct PickedSource {
    /// Full sha of the base commit; the review runs base..head.
    pub base: String,
    /// Head endpoint is the worktree (true) or HEAD (false).
    pub include_worktree: bool,
}

/// A commit as the picker shows it: the engine's summary plus the ref names
/// pointing at it, which are decoration the picker adds.
struct CommitEntry {
    summary: CommitSummary,
    /// Branch/tag names pointing at this commit, for orientation.
    refs: Vec<String>,
}

/// How many commits the picker offers as bases.
const RECENT: usize = 30;

/// Open the picker inside an existing terminal session. `Ok(None)` =
/// cancelled.
pub fn pick_source(
    terminal: &mut super::vendor::terminal::TerminalSession<std::io::Stdout>,
    repo: &Repo,
    theme: &Theme,
) -> anyhow::Result<Option<PickedSource>> {
    // An unborn HEAD has nothing to diff against.
    if !repo.has_commits() {
        anyhow::bail!("no commits yet — commit something first, then review");
    }
    // Whether including the worktree could change anything. Asked once, up
    // front: on a clean tree the snapshot is HEAD's own tree, so the option
    // would cost a full re-hash of every tracked file to produce an identical
    // review — filed under a different identity.
    let dirty = !is_clean(repo)?;

    // HEAD is a legitimate base: with the box ticked it means "just my
    // uncommitted work", so it is NOT skipped.
    let mut commits: Vec<CommitEntry> = repo
        .recent_commits("HEAD", RECENT)?
        .into_iter()
        .map(|summary| CommitEntry {
            summary,
            refs: Vec::new(),
        })
        .collect();

    let refs = repo.refs_by_commit();
    for c in &mut commits {
        if let Some(names) = refs.get(&c.summary.sha) {
            c.refs = names.clone();
        }
    }

    let mut state = PickerState {
        selected: 0,
        // Ticked by default only when it would do something.
        include_worktree: dirty,
        dirty,
        scroll: 0,
    };
    let mut picked: Option<PickedSource> = None;
    let result = loop {
        terminal.draw(|frame| draw(frame, theme, &commits, &mut state))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let key = match event::read()? {
            Event::Key(key) if key.is_press() => key,
            // The wheel is `j`/`k`; a click selects the row under it, and a
            // click on the selected row picks it. The checkbox toggles on a
            // click, as it does on space.
            Event::Mouse(m) if crate::is_input(m.kind) => {
                match m.kind {
                    MouseEventKind::ScrollDown => {
                        state.selected = (state.selected + 1).min(commits.len().saturating_sub(1));
                    }
                    MouseEventKind::ScrollUp => state.selected = state.selected.saturating_sub(1),
                    MouseEventKind::Down(MouseButton::Left) => {
                        match state.hit(m.row, commits.len()) {
                            Some(Hit::Checkbox) => state.include_worktree = !state.include_worktree,
                            Some(Hit::Commit(i)) if i == state.selected => {
                                if let Some(c) = commits.get(i) {
                                    picked = Some(PickedSource {
                                        base: c.summary.sha.clone(),
                                        include_worktree: state.include_worktree,
                                    });
                                }
                                break Ok(());
                            }
                            Some(Hit::Commit(i)) => state.selected = i,
                            None => {}
                        }
                    }
                    _ => {}
                }
                continue;
            }
            _ => continue,
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                state.selected = (state.selected + 1).min(commits.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => state.selected = state.selected.saturating_sub(1),
            // No-op on a clean tree, where the row is not drawn at all.
            KeyCode::Char(' ') if state.dirty => {
                state.include_worktree = !state.include_worktree;
            }
            KeyCode::Enter => {
                if let Some(c) = commits.get(state.selected) {
                    picked = Some(PickedSource {
                        base: c.summary.sha.clone(),
                        include_worktree: state.include_worktree,
                    });
                }
                break Ok(());
            }
            KeyCode::Esc | KeyCode::Char('q') => break Ok(()),
            _ => {}
        }
    };
    result.map(|()| picked)
}

struct PickerState {
    selected: usize,
    include_worktree: bool,
    /// Whether the worktree has anything uncommitted. When false the checkbox
    /// is not drawn and `include_worktree` stays false.
    dirty: bool,
    scroll: usize,
}

/// What a click landed on.
#[derive(Debug, PartialEq, Eq)]
enum Hit {
    Checkbox,
    Commit(usize),
}

impl PickerState {
    /// The rows above the commit list: the checkbox when it is drawn, and the
    /// rule. `header` builds them and asserts this count against what it built.
    fn header_rows(&self) -> usize {
        usize::from(self.dirty) + 1
    }

    /// What screen row `y` holds. The list sits inside the block's top border,
    /// so the first header row is `y == TOP_BORDER`; `scroll` is the one the
    /// last draw settled on, which is the frame the click was aimed at.
    fn hit(&self, y: u16, commits: usize) -> Option<Hit> {
        let line = usize::from(y).checked_sub(TOP_BORDER)?;
        if self.dirty && line == 0 {
            return Some(Hit::Checkbox);
        }
        let i = self.scroll + line.checked_sub(self.header_rows())?;
        (i < commits).then_some(Hit::Commit(i))
    }
}

/// The bar marking rows inside the review. `base..head` EXCLUDES the base,
/// so the bar stops above the selected row; the base itself is the boundary
/// and gets a marker of its own.
const IN_RANGE: &str = "▌ ";
const OUT_RANGE: &str = "  ";
const AT_BASE: &str = "└ ";

/// The block `draw` frames the list with: one row of border at the top, which
/// is where the first header row is, and `BORDER_ROWS` in all.
const TOP_BORDER: usize = 1;
/// Block border, top and bottom.
const BORDER_ROWS: usize = 2 * TOP_BORDER;
/// The blank line plus the key hints under the commit list.
const FOOTER_ROWS: usize = 2;

/// Rows strictly newer than the base are reviewed — the base commit's own
/// changes are not.
fn in_range(row: usize, base: usize) -> bool {
    row < base
}

/// Picking the newest commit as base with uncommitted changes excluded
/// leaves nothing to review.
fn is_empty_range(base: usize, include_worktree: bool) -> bool {
    base == 0 && !include_worktree
}

/// The rows above the commit list. Its length drives the scroll viewport, so
/// it is a function rather than inline pushes — the checkbox row is
/// conditional, and a hard-coded count would mis-scroll the list without it.
fn header(theme: &Theme, state: &PickerState, bar: Style) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();
    // The checkbox, only when there is something to include: itself inside
    // the range when ticked. On a clean tree it would be a no-op that also
    // files the review under a different identity, so it is not offered.
    if state.dirty {
        let (check_bar, check_style) = if state.include_worktree {
            (IN_RANGE, Style::default().fg(theme.header_fg))
        } else {
            (OUT_RANGE, Style::default().fg(theme.gutter_fg))
        };
        let mark = if state.include_worktree { "x" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(check_bar, bar),
            Span::styled(
                format!("[{mark}] uncommitted changes (worktree)"),
                check_style,
            ),
            Span::styled("   space toggles", Style::default().fg(theme.gutter_fg)),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled(
            if state.include_worktree {
                IN_RANGE
            } else {
                OUT_RANGE
            },
            bar,
        ),
        Span::styled(
            "── pick the base: everything after it is reviewed ──",
            Style::default().fg(theme.gutter_fg),
        ),
    ]));

    debug_assert_eq!(
        lines.len(),
        state.header_rows(),
        "the hit test counts the header rows this builds"
    );
    lines
}

/// Rows the chrome occupies around the commit list.
fn chrome_rows(header_rows: usize) -> usize {
    header_rows + BORDER_ROWS + FOOTER_ROWS
}

fn draw(
    frame: &mut ratatui::Frame,
    theme: &Theme,
    commits: &[CommitEntry],
    state: &mut PickerState,
) {
    let area: Rect = frame.area();
    let bar = Style::default().fg(theme.reviewed_fg);

    let mut lines = header(theme, state, bar);

    // Scroll the commit list to keep the cursor visible. The chrome height is
    // DERIVED from the header just built — the checkbox row is conditional, so
    // a hard-coded count would silently mis-scroll the list without it.
    let viewport = (area.height as usize)
        .saturating_sub(chrome_rows(lines.len()))
        .max(1);
    if state.selected < state.scroll {
        state.scroll = state.selected;
    } else if state.selected >= state.scroll + viewport {
        state.scroll = state.selected + 1 - viewport;
    }

    for (i, c) in commits.iter().enumerate().skip(state.scroll).take(viewport) {
        let at_base = i == state.selected;
        let mut style = Style::default().fg(theme.context_fg);
        if at_base {
            style = style.bg(theme.selected_bg).add_modifier(Modifier::BOLD);
        }
        let gutter = if at_base {
            AT_BASE
        } else if in_range(i, state.selected) {
            IN_RANGE
        } else {
            OUT_RANGE
        };
        let mut spans = vec![
            Span::styled(gutter, bar),
            Span::styled(format!("{}  ", c.summary.short), style),
        ];
        if !c.refs.is_empty() {
            spans.push(Span::styled(
                format!("({})  ", c.refs.join(", ")),
                Style::default()
                    .fg(theme.header_fg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(format!("{}  ", c.summary.subject), style));
        spans.push(Span::styled(
            format!("({})", c.summary.author),
            Style::default().fg(theme.gutter_fg),
        ));
        if at_base {
            spans.push(Span::styled(
                "  ← base (excluded)",
                Style::default().fg(theme.gutter_fg),
            ));
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        if state.dirty {
            "  j/k move · space uncommitted · enter review · q cancel"
        } else {
            "  j/k move · enter review · q cancel"
        },
        Style::default().fg(theme.gutter_fg),
    )));

    let head = if state.include_worktree {
        "worktree"
    } else {
        "HEAD"
    };
    let base = commits
        .get(state.selected)
        .map(|c| c.summary.short.as_str())
        .unwrap_or("?");
    let empty = if is_empty_range(state.selected, state.include_worktree) {
        " — nothing to review "
    } else {
        " "
    };
    frame.render_widget(Clear, area);
    // The theme's own ground, under everything. `Clear` resets cells to the
    // TERMINAL's default, so without this a light palette shows the terminal's
    // background wherever nothing else paints — which is most of the screen.
    frame.render_widget(Block::default().style(theme.ground()), area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" dfr review — {base}..{head}{empty}")),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    /// The default palette: these tests are about layout, not colour.
    fn theme() -> super::Theme {
        super::Theme::named(differential_engine::config::ThemeName::Dark)
    }

    fn state(dirty: bool) -> super::PickerState {
        super::PickerState {
            selected: 0,
            include_worktree: dirty,
            dirty,
            scroll: 0,
        }
    }

    /// A click names a screen row; the list is one border and the header
    /// rows down, and `scroll` rows in.
    #[test]
    fn a_click_finds_the_checkbox_and_the_commit_under_it() {
        use super::Hit;
        let mut s = state(true);
        assert_eq!(s.hit(0, 10), None, "the border");
        assert_eq!(s.hit(1, 10), Some(Hit::Checkbox));
        assert_eq!(s.hit(2, 10), None, "the rule");
        assert_eq!(s.hit(3, 10), Some(Hit::Commit(0)));
        s.scroll = 4;
        assert_eq!(s.hit(3, 10), Some(Hit::Commit(4)));
        assert_eq!(s.hit(8, 10), Some(Hit::Commit(9)));
        assert_eq!(s.hit(9, 10), None, "past the last commit");

        let clean = state(false);
        assert_eq!(clean.hit(1, 10), None, "no checkbox: the rule");
        assert_eq!(clean.hit(2, 10), Some(Hit::Commit(0)));
    }

    /// The checkbox is only offered when it would change something.
    #[test]
    fn the_checkbox_row_appears_only_when_the_worktree_is_dirty() {
        let bar = ratatui::style::Style::default();
        assert_eq!(super::header(&theme(), &state(true), bar).len(), 2);
        assert_eq!(super::header(&theme(), &state(false), bar).len(), 1);
        // The hit test counts the same rows without building them.
        for dirty in [true, false] {
            let s = state(dirty);
            assert_eq!(super::header(&theme(), &s, bar).len(), s.header_rows());
        }
    }

    /// The commit list's viewport is derived from the header, so hiding a row
    /// widens it rather than silently mis-scrolling. 6 is what the constant
    /// used to be hard-coded to, back when the header was always two rows.
    #[test]
    fn chrome_height_tracks_the_header() {
        let bar = ratatui::style::Style::default();
        assert_eq!(
            super::chrome_rows(super::header(&theme(), &state(true), bar).len()),
            6
        );
        assert_eq!(
            super::chrome_rows(super::header(&theme(), &state(false), bar).len()),
            5
        );
    }

    #[test]
    fn the_range_excludes_the_base_commit() {
        // base..head is exclusive: with row 3 picked, rows 0-2 (newer
        // commits) are reviewed and row 3 itself is not.
        assert!(super::in_range(0, 3));
        assert!(super::in_range(2, 3));
        assert!(
            !super::in_range(3, 3),
            "the base's own changes are not in the review"
        );
        assert!(
            !super::in_range(4, 3),
            "older commits are not in the review"
        );
        // The newest commit as base reviews nothing on its own...
        assert!(!super::in_range(0, 0));
        assert!(super::is_empty_range(0, false));
        // ...unless uncommitted work is included.
        assert!(!super::is_empty_range(0, true));
        assert!(!super::is_empty_range(1, false));
    }
}
