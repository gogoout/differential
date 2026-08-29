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

use super::theme::Theme;
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

/// What the grouping row says while the agent thinks, after the first slot.
///
/// Slot zero is the agent's own name, built from the backend, so it is not in
/// the table: a const cannot hold a `format!`, and the name is information
/// rather than copy.
///
/// Every line is true, which is the whole constraint on being funny here. The
/// model fetches its own context (ADR 0022); it merges class ids and never
/// hunks, so it cannot lose one (ADR 0001); the response is cached (ADR 0009);
/// and the coverage audit back-fills anything it drops (invariant 5).
const WAITING: [&str; 5] = [
    "it reads the diff so you don't have to",
    "merging class ids — it cannot lose a hunk",
    "cached after this — you only wait once",
    "quicker than reading it in file order",
    "if the model gets bored, the audit notices",
];

/// Seconds a message holds before the next one.
///
/// Long enough to read a line twice, short enough that a minute is not one
/// sentence. The whole set turns over in under half a minute.
const MESSAGE_SECS: u64 = 4;

/// Columns any one line on the stage block may spend after the name field.
///
/// The block is centred on the widest line that CAN appear in it, so an
/// overlong message does not slide the block — it widens it, and on a narrow
/// pane that pushes the whole thing off the left. Hence a cap, and a test.
const LINE_BUDGET: usize = 44;

/// The budget has to leave the block inside a narrow terminal, or it is capping
/// the copy against nothing. 70 columns, less the two the borders take.
const _: () = assert!(STAGE_LEAD + LINE_BUDGET <= 70 - 2);

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
    theme: &Theme,
    rx: Receiver<Progress>,
    worker: &JoinHandle<T>,
) -> anyhow::Result<bool> {
    let started = Instant::now();
    let mut current = 0usize;
    // Set once the grouping stage reports which backend it is waiting on.
    let mut agent: Option<(String, bool)> = None;
    // When that stage began. The rotation counts from HERE, not from `started`:
    // enumerate and classify run first, so a rotation on the total elapsed time
    // is already mid-cycle by the moment the row it belongs to appears, and the
    // reviewer never sees it open on the agent's name.
    let mut asking_since: Option<Instant> = None;
    let mut tick = 0usize;

    loop {
        // Drain everything published since the last frame.
        loop {
            match rx.try_recv() {
                Ok(p) => {
                    if let Progress::Grouping { backend, cached } = &p {
                        agent = Some((backend.clone(), *cached));
                        asking_since = Some(Instant::now());
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

        let waited = asking_since.map_or(Duration::ZERO, |t| t.elapsed());
        terminal.draw(|frame| {
            draw(
                frame,
                theme,
                current,
                agent.as_ref(),
                started.elapsed(),
                waited,
                tick,
            )
        })?;
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
    theme: &Theme,
) -> anyhow::Result<()> {
    terminal.draw(|frame| {
        let area = frame.area();
        frame.render_widget(Clear, area);
        frame.render_widget(Block::default().style(theme.ground()), area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::default(),
                Line::from(Span::styled(
                    "  cancelling — stopping the agent…",
                    Style::default().fg(theme.gutter_fg),
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
/// Measured from every string that is fixed at compile time — the stage
/// descriptions AND the waiting messages — and never from the line currently
/// on screen. That is the whole point: the grouping row's text changes while
/// you watch it, and an indent measured from the live string would slide the
/// block sideways underneath the reader, which reads as a glitch.
///
/// The waiting messages belong in the measurement precisely because they are
/// const. Leaving them out would keep the indent stable but let a long message
/// run off the right edge, so the block would be centred on a width it does not
/// occupy. A backend name longer than all of them still runs on to the right;
/// that one is not ours to cap.
fn stage_width() -> usize {
    STAGE_LEAD
        + STAGES
            .iter()
            .map(|(_, what)| what.chars().count())
            .chain(WAITING.iter().map(|m| m.chars().count()))
            .max()
            .unwrap_or(0)
}

/// What the grouping row says, `waited` into the AGENT CALL.
///
/// Not into the run. Enumerate and classify go first, so a rotation keyed to
/// the total elapsed time is already several slots deep by the time this row
/// has anything to say, and slot zero — the one that names the agent — would
/// be the one slot a reviewer never sees.
///
/// Slot zero names the agent, so a reviewer glancing up in the first four
/// seconds of the call — or once every twenty-four after that — learns which
/// one is thinking. The rest is something to read while it does.
fn waiting_line(backend: &str, waited: Duration) -> String {
    let slot = (waited.as_secs() / MESSAGE_SECS) as usize % (WAITING.len() + 1);
    match slot.checked_sub(1) {
        None => format!("asking {backend}"),
        Some(i) => WAITING[i].to_string(),
    }
}

/// The logo's rows, indented so the block sits centred in `width` columns.
///
/// Empty when the pane cannot hold it: too narrow and the art wraps, too short
/// and it pushes the stages — the thing the reader is actually waiting on —
/// off the bottom. A wordmark is worth a pane's room only when there is room.
fn logo_lines(theme: &Theme, width: usize, height: usize) -> Vec<Line<'static>> {
    if width < LOGO_WIDTH || height < LOGO.len() + STAGES.len() + 3 {
        return Vec::new();
    }
    let pad = indent(width, LOGO_WIDTH);
    LOGO.iter()
        .map(|l| {
            Line::from(Span::styled(
                format!("{pad}{l}"),
                Style::default().fg(theme.header_fg),
            ))
        })
        .collect()
}

fn draw(
    frame: &mut ratatui::Frame,
    theme: &Theme,
    current: usize,
    agent: Option<&(String, bool)>,
    elapsed: Duration,
    waited: Duration,
    tick: usize,
) {
    let area: Rect = frame.area();
    let spin = SPINNER[tick % SPINNER.len()];
    // The block's borders take one column and one row on each side.
    let inner = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = logo_lines(theme, inner, area.height.saturating_sub(2) as usize);
    lines.push(Line::default());
    // The stages and the footer share this one indent, so the footer lines up
    // with the column the stage names start in and neither moves as the
    // elapsed count gains a digit.
    let pad = indent(inner, stage_width());

    for (i, (name, what)) in STAGES.iter().enumerate() {
        let (glyph, style) = match i.cmp(&current) {
            std::cmp::Ordering::Less => ("✓".to_string(), Style::default().fg(theme.reviewed_fg)),
            std::cmp::Ordering::Equal => (
                spin.to_string(),
                Style::default()
                    .fg(theme.header_fg)
                    .add_modifier(Modifier::BOLD),
            ),
            std::cmp::Ordering::Greater => (" ".to_string(), Style::default().fg(theme.gutter_fg)),
        };
        let mut detail = what.to_string();
        // The slow stage says which agent it is waiting on, and whether the
        // grouping cache spared the call. On a miss it is the only row anyone
        // is watching for a minute or more, so it rotates rather than holding
        // one sentence; a cache hit does not wait, so it does not.
        //
        // Only while it is the ACTIVE stage. `asking_since` is never cleared —
        // the elapsed wait is still wanted after the fact — so without this a
        // ticked, finished row would go on rotating, and "if the model gets
        // bored, the audit notices" beside a ✓ reads as still running. Every
        // other completed row falls back to its static description; so does
        // this one.
        if i == 2
            && i == current
            && let Some((backend, cached)) = agent
        {
            detail = if *cached {
                "cached grouping (no agent call)".to_string()
            } else {
                waiting_line(backend, waited)
            };
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{pad}  {glyph} "), style),
            Span::styled(format!("{name:<10}"), style),
            Span::styled(detail, Style::default().fg(theme.gutter_fg)),
        ]));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        format!(
            "{pad}  {:.0}s elapsed · q cancels (stops the agent too)",
            elapsed.as_secs_f64()
        ),
        Style::default().fg(theme.gutter_fg),
    )));

    frame.render_widget(Clear, area);
    frame.render_widget(Block::default().style(theme.ground()), area);
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

    /// The default palette: these tests are about layout, not colour.
    fn theme() -> Theme {
        Theme::named(differential_engine::config::ThemeName::Dark)
    }

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn the_logo_is_centred_as_one_block() {
        let lines = logo_lines(&theme(), 80, 40);
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

    /// The splash drawn `w` columns wide with `stage` active, one `String` per
    /// screen row.
    fn screen(
        w: u16,
        stage: usize,
        agent: Option<&(String, bool)>,
        waited: Duration,
    ) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut t = Terminal::new(TestBackend::new(w, 16)).unwrap();
        // A total elapsed time deliberately unlike `waited`, so a draw that
        // rotated on the wrong clock shows up here rather than passing.
        let elapsed = waited + Duration::from_secs(MESSAGE_SECS * 2 + 1);
        t.draw(|f| draw(f, &theme(), stage, agent, elapsed, waited, 0))
            .unwrap();
        t.backend()
            .buffer()
            .content
            .chunks(w as usize)
            .map(|row| row.iter().map(|c| c.symbol()).collect())
            .collect()
    }

    /// The column each stage NAME starts in.
    ///
    /// The name, not the first visible character: a pending stage's glyph is a
    /// space, so "first non-blank" would report a column two cells to the right
    /// and call an aligned block crooked.
    ///
    /// Each name is looked for in ITS OWN row, found by anchoring on the first
    /// stage's. Scanning every row for every name reported the grouping row as
    /// the "order" row the day a waiting message ended in the word "order" — a
    /// helper that finds the wrong row calls a straight block crooked too.
    fn name_columns(stage: usize, agent: Option<&(String, bool)>, waited: Duration) -> Vec<usize> {
        let rows = screen(80, stage, agent, waited);
        let base = rows
            .iter()
            .position(|r| r.contains(STAGES[0].0))
            .expect("the stage block is not on screen");
        STAGES
            .iter()
            .enumerate()
            .filter_map(|(i, (name, _))| {
                let row = rows.get(base + i)?;
                row.find(name).map(|b| row[..b].chars().count())
            })
            .collect()
    }

    /// The grouping row's text changes when the agent starts, and then again
    /// every few seconds for as long as the wait lasts. The block must not
    /// slide sideways underneath it — a block that moves while you watch it
    /// reads as a glitch, which is why the indent is measured from the const
    /// strings and never from the live line.
    #[test]
    fn the_stage_block_does_not_move_as_the_grouping_line_changes() {
        let agent = ("some-agent-with-a-long-name".to_string(), false);
        let before = name_columns(1, None, Duration::ZERO);
        assert_eq!(before.len(), STAGES.len());
        // And it is a block: every row starts in the same column.
        assert!(before.windows(2).all(|w| w[0] == w[1]), "{before:?}");
        // Once through the whole rotation, and a little past it.
        for secs in 0..=(MESSAGE_SECS * (WAITING.len() as u64 + 2)) {
            let during = name_columns(2, Some(&agent), Duration::from_secs(secs));
            assert_eq!(before, during, "the stage block shifted at {secs}s");
        }
    }

    /// Slot zero names the agent, so a reviewer glancing up early learns which
    /// one is thinking; the rest of the cycle is something to read.
    #[test]
    fn the_grouping_line_opens_with_the_agent_then_rotates() {
        let at = |secs| waiting_line("Claude Code", Duration::from_secs(secs));
        assert_eq!(at(0), "asking Claude Code");
        assert_eq!(at(MESSAGE_SECS - 1), "asking Claude Code");
        assert_eq!(at(MESSAGE_SECS), WAITING[0]);
        assert_eq!(at(MESSAGE_SECS * 2), WAITING[1]);
        // A message holds for its whole slot rather than flickering per frame.
        assert_eq!(at(MESSAGE_SECS), at(MESSAGE_SECS * 2 - 1));
        // And the cycle comes back round to the name.
        let cycle = MESSAGE_SECS * (WAITING.len() as u64 + 1);
        assert_eq!(at(cycle), "asking Claude Code");
        assert_eq!(at(cycle + MESSAGE_SECS), WAITING[0]);
    }

    /// The rotation counts from the agent call, not from the splash opening.
    ///
    /// Keyed to the total elapsed time, enumerate and classify spend the first
    /// slots before the grouping row has anything to say, so the row appears
    /// already mid-cycle and slot zero — the one naming the agent — is the one
    /// slot nobody ever sees.
    #[test]
    fn the_rotation_starts_when_the_agent_does_not_when_the_splash_does() {
        let agent = ("Claude Code".to_string(), false);
        // Half a minute into the run, but the agent call has only just begun.
        let opening = screen(80, 2, Some(&agent), Duration::ZERO).join("\n");
        assert!(opening.contains("asking Claude Code"), "{opening}");
        // And it advances on the call's own clock from there.
        let next = screen(80, 2, Some(&agent), Duration::from_secs(MESSAGE_SECS)).join("\n");
        assert!(next.contains(WAITING[0]), "{next}");
    }

    /// A finished row stops talking.
    ///
    /// `asking_since` is never cleared — how long the agent took is still worth
    /// knowing afterwards — so nothing but the active-stage check stops a
    /// ticked row from rotating for the rest of the run. "if the model gets
    /// bored, the audit notices" beside a ✓ reads as still running.
    #[test]
    fn the_grouping_row_stops_rotating_once_the_stage_is_done() {
        let agent = ("Claude Code".to_string(), false);
        let long = Duration::from_secs(MESSAGE_SECS * 3);
        // Still grouping: the rotation is running.
        let during = screen(80, 2, Some(&agent), long).join("\n");
        assert!(during.contains(WAITING[2]), "{during}");
        // Ordering, and past it: the row is ticked and back to its static
        // description, like every other finished stage.
        for stage in [3, STAGES.len()] {
            let after = screen(80, stage, Some(&agent), long).join("\n");
            assert!(after.contains(STAGES[2].1), "at stage {stage}: {after}");
            for line in WAITING {
                assert!(!after.contains(line), "at stage {stage}: {after}");
            }
            assert!(!after.contains("asking Claude Code"), "at stage {stage}");
        }
    }

    /// A cache hit does not wait, so it has nothing to fill. Rotating there
    /// would be motion for its own sake on a row that is already finished.
    #[test]
    fn a_cached_grouping_says_one_thing_and_keeps_saying_it() {
        let cached = ("Claude Code".to_string(), true);
        for secs in [0, 30, 600] {
            let drawn = screen(80, 2, Some(&cached), Duration::from_secs(secs)).join("\n");
            assert!(
                drawn.contains("cached grouping (no agent call)"),
                "at {secs}s: {drawn}"
            );
        }
    }

    /// The block is centred on the widest line that can appear in it, so an
    /// overlong message widens the block rather than sliding it — and on a
    /// narrow pane that pushes the whole thing off the left edge. This is the
    /// test that fails when someone writes a funnier, longer joke.
    #[test]
    fn no_line_on_the_stage_block_outgrows_its_budget() {
        for line in WAITING
            .iter()
            .copied()
            .chain(STAGES.iter().map(|(_, what)| *what))
        {
            let width = line.chars().count();
            assert!(
                width <= LINE_BUDGET,
                "{width} columns, budget is {LINE_BUDGET}: {line:?}"
            );
        }
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
        assert!(logo_lines(&theme(), LOGO_WIDTH - 1, 40).is_empty());
        assert!(!logo_lines(&theme(), LOGO_WIDTH, 40).is_empty());
    }

    #[test]
    fn a_short_pane_keeps_the_stages_and_drops_the_logo() {
        // The stages are what the reader is waiting on. The wordmark yields.
        let need = LOGO.len() + STAGES.len() + 3;
        assert!(logo_lines(&theme(), 80, need - 1).is_empty());
        assert!(!logo_lines(&theme(), 80, need).is_empty());
    }

    /// Not an assertion — the grouping row at each point in the rotation, and
    /// the narrow pane the budget exists for:
    /// `cargo test -p differential-tui --lib -- --ignored --nocapture render_dump_splash`
    #[test]
    #[ignore = "prints the splash for a human to look at"]
    fn render_dump_splash() {
        let agent = ("Claude Code".to_string(), false);
        for slot in 0..=WAITING.len() as u64 {
            let secs = slot * MESSAGE_SECS;
            println!("\n=== {secs}s ===");
            for row in screen(90, 2, Some(&agent), Duration::from_secs(secs)) {
                println!("{row}");
            }
        }
        println!("\n=== 70 columns, the width the budget is for ===");
        for row in screen(70, 2, Some(&agent), Duration::from_secs(MESSAGE_SECS * 5)) {
            println!("{row}");
        }
    }
}
