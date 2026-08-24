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

/// Draw the splash until the worker finishes. `Ok(Some(()))` means the
/// pipeline is done (join it for the result); `Ok(None)` means the user
/// cancelled with `q`/Esc.
pub fn run<T>(
    terminal: &mut vendor::terminal::TerminalSession<Stdout>,
    rx: Receiver<Progress>,
    worker: &JoinHandle<T>,
) -> anyhow::Result<Option<()>> {
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
            return Ok(Some(()));
        }

        terminal.draw(|frame| draw(frame, current, agent.as_ref(), started, tick))?;
        tick = tick.wrapping_add(1);

        if event::poll(Duration::from_millis(120))?
            && let Event::Key(key) = event::read()?
            && key.is_press()
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        {
            return Ok(None);
        }
    }
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
    let mut lines: Vec<Line> = vec![Line::default()];

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
            Span::styled(format!("  {glyph} "), style),
            Span::styled(format!("{name:<10}"), style),
            Span::styled(detail, Style::default().fg(THEME.gutter_fg)),
        ]));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!(
            "  {:.0}s elapsed · q cancels",
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
