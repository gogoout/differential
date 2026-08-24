//! The terminal reviewer (`dfr review`) over a grouped, ordered document.
//! Library crate: the `dfr` binary lives in `crates/cli` (ADR 0018).
//!
//! One terminal session spans the whole surface: picker → splash (while the
//! pipeline runs on a worker thread) → reviewer. The application layer owns
//! what the pipeline IS — it passes a closure — and this crate owns the
//! screen.

pub mod app;
pub mod picker;
pub mod rows;
pub mod splash;
pub mod theme;
pub mod vendor;

use std::io::Stdout;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{self, Event};
use differential_engine::gitio::Repo;
use differential_engine::grouping::Progress;
use differential_engine::{PipelineOutput, ReviewSession};

use app::{App, Effect};
use picker::PickedSource;
use rows::RowFactory;

type Session = vendor::terminal::TerminalSession<Stdout>;

/// A pipeline result plus the review's IDENTITY — the head AS TYPED keeps a
/// branch review stable while its tip moves, and uncommitted reviews key on a
/// real sha plus a stable literal (ADR 0017).
pub struct Prepared {
    pub out: PipelineOutput,
    pub review_base: String,
    pub head_spec: String,
}

/// Run the whole review surface. `pick` opens the source picker first and
/// hands the choice to `pipeline`; otherwise `pipeline` gets `None` (the user
/// typed a range). `pipeline` runs on a worker thread while the splash reports
/// the stages it publishes on the channel.
pub fn review<P>(repo: &Repo, pick: bool, pipeline: P) -> anyhow::Result<()>
where
    P: FnOnce(Option<PickedSource>, mpsc::Sender<Progress>) -> anyhow::Result<Prepared>
        + Send
        + 'static,
{
    // One terminal guard (vendored, Drop-safe) + one chained panic hook for
    // the whole surface.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        vendor::terminal::restore_stdio_best_effort();
        original_hook(info);
    }));
    let mut terminal = vendor::terminal::TerminalFeatures::new()
        .mouse_enabled(false)
        .keyboard_enhancements_supported(false)
        .enter(std::io::stdout())?;

    let result = review_in(&mut terminal, repo, pick, pipeline);
    terminal.restore()?;
    result
}

fn review_in<P>(terminal: &mut Session, repo: &Repo, pick: bool, pipeline: P) -> anyhow::Result<()>
where
    P: FnOnce(Option<PickedSource>, mpsc::Sender<Progress>) -> anyhow::Result<Prepared>
        + Send
        + 'static,
{
    let picked = if pick {
        match picker::pick_source(terminal, repo)? {
            Some(p) => Some(p),
            None => return Ok(()), // cancelled
        }
    } else {
        None
    };

    // The pipeline (an LLM call on a cache miss) runs off the UI thread so the
    // splash can report what it is waiting on.
    let (tx, rx) = mpsc::channel();
    let worker = std::thread::spawn(move || pipeline(picked, tx));
    let prepared = splash::run(terminal, rx, &worker)?;
    let prepared = match prepared {
        Some(()) => worker
            .join()
            .map_err(|_| anyhow::anyhow!("pipeline thread panicked"))??,
        None => return Ok(()), // cancelled at the splash
    };

    let doc = prepared
        .out
        .document
        .context("invariants failed; nothing to review")?;
    let session = ReviewSession::open(
        repo,
        &prepared.review_base,
        &prepared.head_spec,
        doc,
        prepared.out.view,
    )?;
    let factory = RowFactory::new(
        repo.clone(),
        prepared.out.base.clone(),
        prepared.out.head.clone(),
    );
    run_app(terminal, App::new(session, factory))
}

/// The reviewer's event loop.
fn run_app(terminal: &mut Session, mut app: App) -> anyhow::Result<()> {
    let mut clipboard: Option<arboard::Clipboard> = arboard::Clipboard::new().ok();
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| app.draw(frame))?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        // Burst-drain queued events (trackpad scrolls) into one repaint.
        let mut effects = Vec::new();
        let mut drained = 0;
        loop {
            match event::read()? {
                Event::Key(key) if key.is_press() => {
                    effects.extend(app.handle_key(key));
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                _ => {}
            }
            drained += 1;
            if drained >= 32 || !event::poll(Duration::ZERO)? {
                break;
            }
        }

        let mut quit = false;
        for e in effects {
            match e {
                Effect::Quit => quit = true,
                Effect::CopySummary(text) => {
                    let ok = clipboard
                        .as_mut()
                        .and_then(|c| c.set_text(text.clone()).ok())
                        .is_some();
                    app.status = if ok {
                        "findings summary copied to clipboard".into()
                    } else {
                        "clipboard unavailable".into()
                    };
                }
            }
        }
        if quit {
            return Ok(());
        }
    }
}
