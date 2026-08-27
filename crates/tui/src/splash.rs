//! The splash shown while the pipeline runs.
//!
//! Grouping shells out to an agent on a cache miss, which can take a minute;
//! without this the terminal looked hung between picking a source and the
//! reviewer opening. Stages arrive on a channel from the worker thread.

use std::io::Stdout;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode};
use differential_engine::grouping::Progress;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::theme::THEME;
use super::vendor;

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// The wordmark, shown above the stages.
///
/// Drawn by prefixing every line with the SAME indent, never by centring each
/// line on its own width: the lines are different lengths and their leading
/// spaces are part of the letters, so a per-line centre would shear the block
/// apart.
const LOGO: [&str; 5] = [
    r"       ___ ________                     __  _       __",
    r"  ____/ (_) __/ __/__  ________  ____  / /_(_)___ _/ /",
    r" / __  / / /_/ /_/ _ \/ ___/ _ \/ __ \/ __/ / __ `/ /",
    r"/ /_/ / / __/ __/  __/ /  /  __/ / / / /_/ / /_/ / /",
    r"\__,_/_/_/ /_/  \___/_/   \___/_/ /_/\__/_/\__,_/_/",
];

/// Columns the widest logo line occupies.
///
/// Characters, not bytes. They coincide while the art is ASCII, and the art is
/// ASCII deliberately: the connected box-drawing diagonals (`╱`, `╲`) are East
/// Asian Ambiguous, which a terminal pins to one cell but a browser does not —
/// so the same wordmark sheared apart in the README's code block. One version,
/// correct in both, beats two that drift.
const LOGO_WIDTH: usize = 54;

/// Columns a stage row spends before its description: two of margin, the
/// spinner or tick, a space, and the ten-column name field.
const STAGE_LEAD: usize = 2 + 1 + 1 + 10;

/// The stages a reviewer sees, in order. `Done` is not displayed — it ends the
/// splash.
const STAGES: [(&str, &str); 4] = [
    ("enumerate", "reading every file in the range"),
    ("classify", "partitioning hunks into shape classes"),
    ("group", "labelling the reading plan"),
    ("order", "foundation-first arrangement"),
];

fn stage_index(p: &Progress) -> usize {
    match p {
        Progress::Enumerating => 0,
        Progress::Classifying => 1,
        Progress::Grouping { .. } => 2,
        Progress::Ordering => 3,
        Progress::Done => STAGES.len(),
    }
}

/// Draw the splash until the worker finishes. `true` means the pipeline is
/// done (join it for the result); `false` means the user cancelled with
/// `q`/Esc and the caller must stop the work.
pub fn run<T>(
    terminal: &mut vendor::terminal::TerminalSession<Stdout>,
    rx: Receiver<Progress>,
    worker: &JoinHandle<T>,
) -> anyhow::Result<bool> {
    let started = Instant::now();
    let mut current = 0usize;
    // Set once the grouping stage reports which backend it is waiting on.
    let mut agent: Option<(String, bool)> = None;
    let mut tick = 0usize;

    loop {
        // Drain everything published since the last frame.
        loop {
            match rx.try_recv() {
                Ok(p) => {
                    if let Progress::Grouping { backend, cached } = &p {
                        agent = Some((backend.clone(), *cached));
                    }
                    current = stage_index(&p);
                }
                Err(TryRecvError::Empty) => break,
                // The worker dropped the sender: it is finishing up.
                Err(TryRecvError::Disconnected) => break,
            }
        }
        if worker.is_finished() {
            return Ok(true);
        }

        terminal.draw(|frame| draw(frame, current, agent.as_ref(), started, tick))?;
        tick = tick.wrapping_add(1);

        if event::poll(Duration::from_millis(120))?
            && let Event::Key(key) = event::read()?
            && key.is_press()
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        {
            return Ok(false);
        }
    }
}

/// Shown while an abandoned run is being torn down — killing the agent
/// subprocess takes a moment, and a frozen screen would look like a hang.
pub fn draw_cancelling(
    terminal: &mut vendor::terminal::TerminalSession<Stdout>,
) -> anyhow::Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::default(),
                Line::from(Span::styled(
                    "  cancelling — stopping the agent…",
                    Style::default().fg(THEME.gutter_fg),
                )),
            ])
            .block(Block::default().borders(Borders::ALL).title(" dfr review ")),
            area,
        );
    })?;
    Ok(())
}

/// The indent that centres a `block` of columns inside `width`.
///
/// One indent for the whole block, never a centre per line. The stage rows
/// share a glyph column and a name column, and the logo's leading spaces are
/// part of its letters — centring each line on its own width would shear
/// either of them apart. Empty when the block does not fit; it then runs from
/// the left rather than off both edges.
fn indent(width: usize, block: usize) -> String {
    " ".repeat(width.saturating_sub(block) / 2)
}

/// Columns the stage block occupies.
///
/// Measured from the STATIC descriptions only. The grouping row's text changes
/// when the agent starts, and measuring the live string would slide the whole
/// block sideways mid-run — a block that moves while you watch it reads as a
/// glitch. A backend name longer than the widest description simply runs on to
/// the right.
fn stage_width() -> usize {
    STAGE_LEAD
        + STAGES
            .iter()
            .map(|(_, what)| what.chars().count())
            .max()
            .unwrap_or(0)
}

/// The logo's rows, indented so the block sits centred in `width` columns.
///
/// Empty when the pane cannot hold it: too narrow and the art wraps, too short
/// and it pushes the stages — the thing the reader is actually waiting on —
/// off the bottom. A wordmark is worth a pane's room only when there is room.
fn logo_lines(width: usize, height: usize) -> Vec<Line<'static>> {
    if width < LOGO_WIDTH || height < LOGO.len() + STAGES.len() + 3 {
        return Vec::new();
    }
    let pad = indent(width, LOGO_WIDTH);
    LOGO.iter()
        .map(|l| {
            Line::from(Span::styled(
                format!("{pad}{l}"),
                Style::default().fg(THEME.header_fg),
            ))
        })
        .collect()
}

fn draw(
    frame: &mut ratatui::Frame,
    current: usize,
    agent: Option<&(String, bool)>,
    started: Instant,
    tick: usize,
) {
    let area: Rect = frame.area();
    let spin = SPINNER[tick % SPINNER.len()];
    // The block's borders take one column and one row on each side.
    let inner = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = logo_lines(inner, area.height.saturating_sub(2) as usize);
    lines.push(Line::default());
    // The stages and the footer share this one indent, so the footer lines up
    // with the column the stage names start in and neither moves as the
    // elapsed count gains a digit.
    let pad = indent(inner, stage_width());

    for (i, (name, what)) in STAGES.iter().enumerate() {
        let (glyph, style) = match i.cmp(&current) {
            std::cmp::Ordering::Less => ("✓".to_string(), Style::default().fg(THEME.reviewed_fg)),
            std::cmp::Ordering::Equal => (
                spin.to_string(),
                Style::default()
                    .fg(THEME.header_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            std::cmp::Ordering::Greater => (" ".to_string(), Style::default().fg(THEME.gutter_fg)),
        };
        let mut detail = what.to_string();
        // The slow stage says which agent it is waiting on, and whether the
        // grouping cache spared the call.
        if i == 2
            && let Some((backend, cached)) = agent
        {
            detail = if *cached {
                "cached grouping (no agent call)".to_string()
            } else {
                format!("asking {backend}")
            };
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{pad}  {glyph} "), style),
            Span::styled(format!("{name:<10}"), style),
            Span::styled(detail, Style::default().fg(THEME.gutter_fg)),
        ]));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!(
            "{pad}  {:.0}s elapsed · q cancels (stops the agent too)",
            started.elapsed().as_secs_f64()
        ),
        Style::default().fg(THEME.gutter_fg),
    )));

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" preparing the review "),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn the_logo_is_centred_as_one_block() {
        let lines = logo_lines(80, 40);
        let drawn = text(&lines);
        assert_eq!(drawn.len(), LOGO.len());
        // ONE offset, applied to every row untouched. The art's own leading
        // spaces are part of the letters, so the only correct transform is a
        // uniform prefix — centring each line on its own width would shear
        // the block apart.
        let pad = " ".repeat((80 - LOGO_WIDTH) / 2);
        let want: Vec<String> = LOGO.iter().map(|l| format!("{pad}{l}")).collect();
        assert_eq!(drawn, want);
        // Centred means the room left over is split evenly, give or take the
        // odd column.
        let widest = drawn.iter().map(|l| l.chars().count()).max().unwrap();
        assert!(80 - widest <= pad.len() + 1, "the block sits off-centre");
    }

    /// The column each stage NAME starts in, for a given stage and agent line.
    ///
    /// The name, not the first visible character: a pending stage's glyph is a
    /// space, so "first non-blank" would report a column two cells to the
    /// right and call an aligned block crooked.
    fn name_columns(stage: usize, agent: Option<&(String, bool)>) -> Vec<usize> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut t = Terminal::new(TestBackend::new(80, 16)).unwrap();
        t.draw(|f| draw(f, stage, agent, Instant::now(), 0))
            .unwrap();
        let rows: Vec<String> = t
            .backend()
            .buffer()
            .content
            .chunks(80)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect();
        STAGES
            .iter()
            .filter_map(|(name, _)| {
                rows.iter()
                    .find_map(|row| row.find(name).map(|b| row[..b].chars().count()))
            })
            .collect()
    }

    /// The grouping row's text changes when the agent starts. The block must
    /// not slide sideways underneath it — a block that moves while you watch
    /// it reads as a glitch, which is why the indent is measured from the
    /// static descriptions and never from the live line.
    #[test]
    fn the_stage_block_does_not_move_when_the_agent_line_changes() {
        let agent = ("some-agent-with-a-long-name".to_string(), false);
        let before = name_columns(1, None);
        let during = name_columns(2, Some(&agent));
        assert_eq!(before.len(), STAGES.len());
        assert_eq!(before, during, "the stage block shifted mid-run");
        // And it is a block: every row starts in the same column.
        assert!(before.windows(2).all(|w| w[0] == w[1]), "{before:?}");
    }

    /// The art and the constant have to agree, or every offset is wrong by the
    /// difference. Characters, not bytes: the diagonals are box-drawing glyphs.
    #[test]
    fn the_logo_is_as_wide_as_it_says() {
        let widest = LOGO.iter().map(|l| l.chars().count()).max().unwrap();
        assert_eq!(widest, LOGO_WIDTH);
    }

    #[test]
    fn a_pane_too_narrow_for_the_logo_gets_none() {
        // One column short is still short: art that wraps is worse than none.
        assert!(logo_lines(LOGO_WIDTH - 1, 40).is_empty());
        assert!(!logo_lines(LOGO_WIDTH, 40).is_empty());
    }

    #[test]
    fn a_short_pane_keeps_the_stages_and_drops_the_logo() {
        // The stages are what the reader is waiting on. The wordmark yields.
        let need = LOGO.len() + STAGES.len() + 3;
        assert!(logo_lines(80, need - 1).is_empty());
        assert!(!logo_lines(80, need).is_empty());
    }
}
