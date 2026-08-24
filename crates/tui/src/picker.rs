//! The source picker behind bare `dfr review`.
//!
//! You tick "include uncommitted changes" and pick a BASE commit; the review
//! runs from that commit to either the worktree (ticked) or HEAD. A leading
//! bar marks the rows inside the selected range, so what is covered is
//! visible while choosing.

use std::collections::HashMap;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use differential_engine::gitio::Repo;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::theme::THEME;

/// What the user picked: a base commit, plus whether uncommitted work is in.
pub struct PickedSource {
    /// Full sha of the base commit; the review runs base..head.
    pub base: String,
    /// Head endpoint is the worktree (true) or HEAD (false).
    pub include_worktree: bool,
}

struct CommitEntry {
    sha: String,
    short: String,
    subject: String,
    author: String,
    /// Branch/tag names pointing at this commit, for orientation.
    refs: Vec<String>,
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
                    refs: Vec::new(),
                }),
                _ => None,
            }
        })
        .collect()
}

/// `for-each-ref --format='%(objectname)%x00%(*objectname)%x00%(refname:short)'`
/// output → sha -> ref names. Plumbing, so unaffected by log.decorate config;
/// annotated tags carry the peeled commit in the second field.
fn parse_refs(bytes: &[u8]) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for line in bytes.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        let fields: Vec<String> = line
            .split(|&b| b == 0)
            .map(|f| String::from_utf8_lossy(f).into_owned())
            .collect();
        let [oid, peeled, name] = fields.as_slice() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        // An annotated tag's own object id is the tag; the commit it points at
        // is the peeled one.
        let target = if peeled.is_empty() { oid } else { peeled };
        out.entry(target.clone()).or_default().push(name.clone());
    }
    out
}

/// Open the picker inside an existing terminal session. `Ok(None)` =
/// cancelled.
pub fn pick_source(
    terminal: &mut super::vendor::terminal::TerminalSession<std::io::Stdout>,
    repo: &Repo,
) -> anyhow::Result<Option<PickedSource>> {
    // An unborn HEAD has nothing to diff against.
    if repo.rev_parse("HEAD").is_err() {
        anyhow::bail!("no commits yet — commit something first, then review");
    }
    // HEAD is a legitimate base: with the box ticked it means "just my
    // uncommitted work", so it is NOT skipped.
    let raw = repo.run(
        [
            "rev-list",
            "--max-count=30",
            "--no-commit-header",
            "--format=%H%x00%h%x00%s%x00%an",
            "HEAD",
        ],
        None,
    )?;
    let mut commits = parse_rev_list(&raw);

    let refs = repo
        .run(
            [
                "for-each-ref",
                "--format=%(objectname)%x00%(*objectname)%x00%(refname:short)",
                "refs/heads",
                "refs/tags",
                "refs/remotes",
            ],
            None,
        )
        .map(|out| parse_refs(&out))
        .unwrap_or_default();
    for c in &mut commits {
        if let Some(names) = refs.get(&c.sha) {
            c.refs = names.clone();
        }
    }

    let mut state = PickerState {
        selected: 0,
        include_worktree: true,
        scroll: 0,
    };
    let mut picked: Option<PickedSource> = None;
    let result = loop {
        terminal.draw(|frame| draw(frame, &commits, &mut state))?;
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
                state.selected = (state.selected + 1).min(commits.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => state.selected = state.selected.saturating_sub(1),
            KeyCode::Char(' ') => state.include_worktree = !state.include_worktree,
            KeyCode::Enter => {
                if let Some(c) = commits.get(state.selected) {
                    picked = Some(PickedSource {
                        base: c.sha.clone(),
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
    scroll: usize,
}

/// The bar marking rows inside the review: everything newer than the base,
/// down to and including the base row itself.
const IN_RANGE: &str = "▌ ";
const OUT_RANGE: &str = "  ";

fn draw(frame: &mut ratatui::Frame, commits: &[CommitEntry], state: &mut PickerState) {
    let area: Rect = frame.area();
    let bar = Style::default().fg(THEME.reviewed_fg);
    let mut lines: Vec<Line> = Vec::new();

    // The checkbox: itself inside the range when ticked.
    let (check_bar, check_style) = if state.include_worktree {
        (IN_RANGE, Style::default().fg(THEME.header_fg))
    } else {
        (OUT_RANGE, Style::default().fg(THEME.gutter_fg))
    };
    let mark = if state.include_worktree { "x" } else { " " };
    lines.push(Line::from(vec![
        Span::styled(check_bar, bar),
        Span::styled(
            format!("[{mark}] uncommitted changes (worktree)"),
            check_style,
        ),
        Span::styled("   space toggles", Style::default().fg(THEME.gutter_fg)),
    ]));
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
            Style::default().fg(THEME.gutter_fg),
        ),
    ]));

    // Scroll the commit list to keep the cursor visible; 4 chrome lines
    // (2 header + border top/bottom) plus the footer.
    let viewport = (area.height as usize).saturating_sub(6).max(1);
    if state.selected < state.scroll {
        state.scroll = state.selected;
    } else if state.selected >= state.scroll + viewport {
        state.scroll = state.selected + 1 - viewport;
    }

    for (i, c) in commits.iter().enumerate().skip(state.scroll).take(viewport) {
        // In range: every commit newer than the base, and the base itself.
        let in_range = i <= state.selected;
        let mut style = Style::default().fg(THEME.context_fg);
        if i == state.selected {
            style = style.bg(THEME.selected_bg).add_modifier(Modifier::BOLD);
        }
        let mut spans = vec![
            Span::styled(if in_range { IN_RANGE } else { OUT_RANGE }, bar),
            Span::styled(format!("{}  ", c.short), style),
        ];
        if !c.refs.is_empty() {
            spans.push(Span::styled(
                format!("({})  ", c.refs.join(", ")),
                Style::default()
                    .fg(THEME.header_fg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(format!("{}  ", c.subject), style));
        spans.push(Span::styled(
            format!("({})", c.author),
            Style::default().fg(THEME.gutter_fg),
        ));
        lines.push(Line::from(spans));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "  j/k move · space uncommitted · enter review · q cancel",
        Style::default().fg(THEME.gutter_fg),
    )));

    let head = if state.include_worktree {
        "worktree"
    } else {
        "HEAD"
    };
    let base = commits
        .get(state.selected)
        .map(|c| c.short.as_str())
        .unwrap_or("?");
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" dfr review — {base}..{head} ")),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::{parse_refs, parse_rev_list};

    #[test]
    fn parses_nul_separated_records() {
        let raw = b"aaaa\0a1\0fix the thing\0Alice\nbbbb\0b2\0subject with \xe2\x9c\x93 unicode\0B\xc3\xb6b\n";
        let entries = parse_rev_list(raw);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sha, "aaaa");
        assert_eq!(entries[0].short, "a1");
        assert_eq!(entries[0].subject, "fix the thing");
        assert_eq!(entries[0].author, "Alice");
        assert!(entries[0].refs.is_empty());
        assert_eq!(entries[1].subject, "subject with ✓ unicode");
        assert_eq!(entries[1].author, "Böb");
    }

    #[test]
    fn tolerates_empty_and_malformed_lines() {
        assert!(parse_rev_list(b"").is_empty());
        assert!(parse_rev_list(b"\n\n").is_empty());
        assert!(parse_rev_list(b"only-two\0fields\n").is_empty());
    }

    #[test]
    fn refs_group_by_commit_and_peel_annotated_tags() {
        // Lightweight ref: own oid is the commit. Annotated tag: the peeled
        // field carries the commit.
        let raw = b"aaaa\0\0main\naaaa\0\0origin/main\ntagobj\0aaaa\0v1.0\nbbbb\0\0feature\n";
        let refs = parse_refs(raw);
        assert_eq!(
            refs.get("aaaa").unwrap(),
            &vec![
                "main".to_string(),
                "origin/main".to_string(),
                "v1.0".to_string()
            ]
        );
        assert_eq!(refs.get("bbbb").unwrap(), &vec!["feature".to_string()]);
        // The tag object's own id is never a key.
        assert!(!refs.contains_key("tagobj"));
    }

    #[test]
    fn refs_tolerate_junk() {
        assert!(parse_refs(b"").is_empty());
        assert!(parse_refs(b"\n\n").is_empty());
        assert!(parse_refs(b"two\0fields\n").is_empty());
        // An empty ref name is skipped rather than stored.
        assert!(parse_refs(b"aaaa\0\0\n").is_empty());
    }
}
