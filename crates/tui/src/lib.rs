//! The terminal reviewer (`dfr review`) over a grouped, ordered document.
//! Library crate: the `dfr` binary lives in `crates/cli` (ADR 0018).
//!
//! One terminal session spans the whole surface: picker → splash (while the
//! pipeline runs on a worker thread) → reviewer. The application layer owns
//! what the pipeline IS — it passes a closure — and this crate owns the
//! screen.

pub mod app;
pub mod osc52;
pub mod picker;
pub mod rows;
pub mod splash;
pub mod theme;
/// Vendored MIT code (tuicr, lumen). PRIVATE: nothing outside this crate uses
/// it, and while it was public the compiler could never tell us which of it
/// was actually reachable — every `pub fn` was exported surface by definition.
mod vendor;
pub mod window;

use std::io::{Stdout, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{self, Event};
use differential_engine::gitio::Repo;
use differential_engine::grouping::Progress;
use differential_engine::plan;
use differential_engine::ports::ReviewIdentity;
use differential_engine::review_identity;
use differential_engine::store::{FsReviewCatalogue, FsReviewStore};
use differential_engine::{PipelineOutput, ReviewSession};

use app::{App, Effect, Viewport};
pub use app::{ForgeLink, ReviewOptions};
use picker::PickedSource;
use ratatui::layout::Rect;
use rows::RowFactory;

type Session = vendor::terminal::TerminalSession<Stdout>;

/// A pipeline result plus the review's IDENTITY — the head AS TYPED keeps a
/// branch review stable while its tip moves, uncommitted reviews key on a real
/// sha plus a stable literal (ADR 0017), and a named session keys on the name
/// alone (ADR 0027).
pub struct Prepared {
    pub out: PipelineOutput,
    pub identity: ReviewIdentity,
    /// The forge the review is of, when it is of a request (ADR 0029). The
    /// reviewer fetches its threads as soon as it opens.
    pub forge: Option<ForgeLink>,
}

/// Run the whole review surface. `pick` opens the source picker first and
/// hands the choice to `pipeline`; otherwise `pipeline` gets `None` (the user
/// typed a range). `pipeline` runs on a worker thread while the splash reports
/// the stages it publishes on the channel.
///
/// `opts` is presentation the app layer read from config; this crate owns the
/// screen, not the configuration.
pub fn review<P>(repo: &Repo, pick: bool, opts: ReviewOptions, pipeline: P) -> anyhow::Result<()>
where
    P: FnOnce(
            Option<PickedSource>,
            mpsc::Sender<Progress>,
            Arc<AtomicBool>,
        ) -> anyhow::Result<Prepared>
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

    let result = review_in(&mut terminal, repo, pick, opts, pipeline);
    terminal.restore()?;
    result
}

fn review_in<P>(
    terminal: &mut Session,
    repo: &Repo,
    pick: bool,
    opts: ReviewOptions,
    pipeline: P,
) -> anyhow::Result<()>
where
    P: FnOnce(
            Option<PickedSource>,
            mpsc::Sender<Progress>,
            Arc<AtomicBool>,
        ) -> anyhow::Result<Prepared>
        + Send
        + 'static,
{
    // Built once, before anything draws: the picker and the splash both paint
    // before `App` exists, and building a palette parses the syntax set.
    let theme = theme::Theme::named(opts.theme);
    let picked = if pick {
        match picker::pick_source(terminal, repo, &theme)? {
            Some(p) => Some(p),
            None => return Ok(()), // cancelled
        }
    } else {
        None
    };

    // The pipeline (an LLM call on a cache miss) runs off the UI thread so the
    // splash can report what it is waiting on.
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker = {
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || pipeline(picked, tx, cancel))
    };
    let finished = splash::run(terminal, &theme, rx, &worker)?;
    if !finished {
        // Cancelling means the agent subprocess dies too, not just that we
        // stop watching it: raise the flag, then wait for the worker to
        // unwind so nothing outlives this process.
        cancel.store(true, Ordering::Relaxed);
        splash::draw_cancelling(terminal, &theme)?;
        let _ = worker.join();
        return Ok(());
    }
    let prepared = worker
        .join()
        .map_err(|_| anyhow::anyhow!("pipeline thread panicked"))??;

    let doc = prepared
        .out
        .document
        .context("invariants failed; nothing to review")?;
    // The renderer is an adapter: it composes the concrete store rather than
    // carrying a generic parameter for a choice it never makes.
    let catalogue = FsReviewCatalogue::new(repo)?;
    let id = review_identity::resolve(&catalogue, repo, &prepared.identity)?;
    // Adoption is silent, but not secret: the marks a reader did not make in
    // this range are the one thing that would otherwise arrive unexplained.
    let adopted = match &prepared.identity {
        ReviewIdentity::Range { base, head_spec } => id != plan::review_id(base, head_spec),
        ReviewIdentity::Named(_) | ReviewIdentity::Remote(_) => false,
    };
    let store = FsReviewStore::for_review(repo, &id)?;
    let session = ReviewSession::open(store, doc, prepared.out.view)?;
    let factory = RowFactory::new(
        repo.clone(),
        prepared.out.base.clone(),
        prepared.out.head.clone(),
    );
    let range = opts.range.clone();
    let mut app = App::new(session, factory, opts, theme);
    if adopted {
        app.status = "resumed the review already open on this branch".into();
    }
    if let Some(link) = prepared.forge {
        app.link_forge(link);
        app.start_fetch();
    }
    run_app(terminal, app, range.as_deref())
}

/// The terminal's current size, as the model wants it.
fn measure() -> anyhow::Result<Viewport> {
    let (w, h) = crossterm::terminal::size()?;
    Ok(Viewport::measure(Rect::new(0, 0, w, h)))
}

/// The reviewer's event loop.
///
/// Geometry is measured and pushed into the model BEFORE any key reaches it,
/// so scroll state is decided in update and `draw` is a pure function of the
/// model.
fn run_app(terminal: &mut Session, mut app: App, range: Option<&str>) -> anyhow::Result<()> {
    let mut clipboard: Option<arboard::Clipboard> = arboard::Clipboard::new().ok();
    // Read once: the multiplexer a session is inside cannot change under it.
    let wrap = osc52::Wrap::detect();
    app.set_viewport(measure()?);
    let mut dirty = true;
    loop {
        // A forge answer lands between keys, so it is looked for on every
        // turn of the loop and not only when a key arrives.
        if app.poll_forge() {
            dirty = true;
        }
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
                // A resize is state, folded in HERE rather than at draw time
                // — and in stream order, so keys before and after it each see
                // the geometry that was true when they were pressed.
                Event::Resize(w, h) => {
                    app.set_viewport(Viewport::measure(Rect::new(0, 0, w, h)));
                    dirty = true;
                }
                // Bracketed paste is enabled precisely so this arrives whole.
                // Dropping it meant a paste into the finding composer did
                // nothing, which reads as the box being broken.
                Event::Paste(text) => {
                    app.handle_paste(&text);
                    dirty = true;
                }
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
                    let copied = clipboard
                        .as_mut()
                        .and_then(|c| c.set_text(text.clone()).ok())
                        .is_some();
                    app.status = if copied {
                        "findings summary copied to clipboard".into()
                    } else {
                        summary_fallback(&text, wrap, range)
                    };
                }
            }
        }
        if quit {
            return Ok(());
        }
    }
}

/// No local clipboard: offer the text to the terminal, and name the command
/// that prints it either way.
///
/// Both, always. `arboard` needs a display server that a remote session does
/// not have, and OSC 52 reaches the reader's own terminal instead — but it is
/// unacknowledged, so a terminal that ignored the sequence looks exactly like
/// one that took it. Naming the command is what makes the feature honest:
/// whatever the terminal did or did not do, the summary is one command away.
fn summary_fallback(text: &str, wrap: osc52::Wrap, range: Option<&str>) -> String {
    let sent = osc52::sequence(text, wrap).is_some_and(|seq| {
        let mut out = std::io::stdout();
        out.write_all(seq.as_bytes())
            .and_then(|()| out.flush())
            .is_ok()
    });
    format!(
        "{} · {}",
        if sent {
            "sent via the terminal"
        } else {
            "clipboard unavailable"
        },
        summary_command(range)
    )
}

/// The command that prints the same text, with the range filled in when the
/// reader typed one.
///
/// The picker leaves no range to name — its worktree source has no spelling
/// `dfr findings` accepts — so the reader is told the flag and supplies the
/// rest, which is still better than being told nothing.
fn summary_command(range: Option<&str>) -> String {
    match range {
        Some(r) => format!("dfr findings {r} --summary"),
        None => "dfr findings <range> --summary".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_footer_names_a_command_the_reader_can_run() {
        assert_eq!(
            summary_command(Some("main..feature")),
            "dfr findings main..feature --summary"
        );
        // No typed range (the picker): the flag still tells them where to look.
        assert_eq!(summary_command(None), "dfr findings <range> --summary");
    }
}
