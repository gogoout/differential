//! The source picker behind bare `dfr review`: uncommitted state (worktree /
//! staged) or "everything since" a recent commit.
//!
//! Runs as its own short terminal session BEFORE the pipeline, so the
//! potentially slow grouping step (an LLM call on a cache miss) happens in
//! normal terminal mode, not frozen inside raw mode.

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use differential_engine::gitio::Repo;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::theme::THEME;
use super::vendor;

pub enum PickedSource {
    /// Review everything since this commit: `<sha>..HEAD`.
    Commit { sha: String },
    /// Index vs HEAD.
    Staged,
    /// Worktree (incl. untracked) vs index.
    Worktree,
}

struct CommitEntry {
    sha: String,
    short: String,
    subject: String,
    author: String,
}

/// One selectable picker row.
enum Item {
    Worktree,
    Staged,
    Commit(usize),
}

/// `rev-list --no-commit-header --format=%H%x00%h%x00%s%x00%an` output: one
/// record per line, fields NUL-separated (subjects are single-line by
/// definition, so the line split is safe; bytes decode lossily).
fn parse_rev_list(bytes: &[u8]) -> Vec<CommitEntry> {
    bytes
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .filter_map(|line| {
            let fields: Vec<String> = line
                .split(|&b| b == 0)
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .collect();
            match fields.as_slice() {
                [sha, short, subject, author] => Some(CommitEntry {
                    sha: sha.clone(),
                    short: short.clone(),
                    subject: subject.clone(),
                    author: author.clone(),
                }),
                _ => None,
            }
        })
        .collect()
}

/// Open the picker. `Ok(None)` = cancelled.
pub fn pick_source(repo: &Repo) -> anyhow::Result<Option<PickedSource>> {
    // An unborn HEAD has nothing to diff against — not even staged review.
    if repo.rev_parse("HEAD").is_err() {
        anyhow::bail!("no commits yet — commit something first, then review");
    }
    // Skip HEAD itself: as a base it would select an empty range, and the
    // uncommitted options above cover "what's newer than HEAD".
    let raw = repo.run(
        [
            "rev-list",
            "--max-count=20",
            "--skip=1",
            "--no-commit-header",
            "--format=%H%x00%h%x00%s%x00%an",
            "HEAD",
        ],
        None,
    )?;
    let commits = parse_rev_list(&raw);

    let mut items = vec![Item::Worktree, Item::Staged];
    items.extend((0..commits.len()).map(Item::Commit));

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        vendor::terminal::restore_stdio_best_effort();
        original_hook(info);
    }));
    let mut terminal = vendor::terminal::TerminalFeatures::new()
        .mouse_enabled(false)
        .keyboard_enhancements_supported(false)
        .enter(std::io::stdout())?;

    let mut selected = 0usize;
    let mut picked: Option<PickedSource> = None;
    let result = loop {
        terminal.draw(|frame| draw(frame, &items, &commits, selected))?;
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if !key.is_press() {
            continue;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                selected = (selected + 1).min(items.len() - 1);
            }
            KeyCode::Char('k') | KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Enter => {
                picked = Some(match items[selected] {
                    Item::Worktree => PickedSource::Worktree,
                    Item::Staged => PickedSource::Staged,
                    Item::Commit(i) => PickedSource::Commit {
                        sha: commits[i].sha.clone(),
                    },
                });
                break Ok(());
            }
            KeyCode::Esc | KeyCode::Char('q') => break Ok(()),
            _ => {}
        }
    };
    terminal.restore()?;
    result.map(|()| picked)
}

fn draw(frame: &mut ratatui::Frame, items: &[Item], commits: &[CommitEntry], selected: usize) {
    let area: Rect = frame.area();
    let mut lines: Vec<Line> = vec![Line::default()];
    for (i, item) in items.iter().enumerate() {
        let text = match item {
            Item::Worktree => {
                "  worktree — all uncommitted changes (staged + unstaged + untracked)".to_string()
            }
            Item::Staged => {
                "  staged   — what `git commit` would record (index vs HEAD)".to_string()
            }
            Item::Commit(c) => {
                let e = &commits[*c];
                format!("  {}  {}  ({})", e.short, e.subject, e.author)
            }
        };
        let mut style = Style::default().fg(THEME.context_fg);
        if i == selected {
            style = style.bg(THEME.selected_bg).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(Span::styled(text, style)));
        // Separator between uncommitted sources and the commit list.
        if matches!(item, Item::Staged) && commits.len() > i {
            lines.push(Line::from(Span::styled(
                "  ── or review everything since a commit (<commit>..HEAD) ──",
                Style::default().fg(THEME.gutter_fg),
            )));
        }
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "  j/k move · enter review · q cancel",
        Style::default().fg(THEME.gutter_fg),
    )));
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" dfr review — pick what to review "),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::parse_rev_list;

    #[test]
    fn parses_nul_separated_records() {
        let raw = b"aaaa\0a1\0fix the thing\0Alice\nbbbb\0b2\0subject with \xe2\x9c\x93 unicode\0B\xc3\xb6b\n";
        let entries = parse_rev_list(raw);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sha, "aaaa");
        assert_eq!(entries[0].short, "a1");
        assert_eq!(entries[0].subject, "fix the thing");
        assert_eq!(entries[0].author, "Alice");
        assert_eq!(entries[1].subject, "subject with ✓ unicode");
        assert_eq!(entries[1].author, "Böb");
    }

    #[test]
    fn tolerates_empty_and_malformed_lines() {
        assert!(parse_rev_list(b"").is_empty());
        assert!(parse_rev_list(b"\n\n").is_empty());
        assert!(parse_rev_list(b"only-two\0fields\n").is_empty());
    }
}
