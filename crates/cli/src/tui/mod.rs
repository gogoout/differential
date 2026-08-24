//! `dfr review` — the terminal reviewer over a grouped, ordered document.

pub mod app;
pub mod picker;
pub mod rows;
pub mod theme;
pub mod vendor;

use std::time::Duration;

use anyhow::Context;
use crossterm::event::{self, Event};
use differential_engine::gitio::Repo;
use differential_engine::{PipelineOutput, ReviewSession};

use app::{App, Effect};
use rows::RowFactory;

/// Open the reviewer. `(review_base, head_spec)` is the review's IDENTITY —
/// the head AS TYPED keeps a branch review stable while its tip moves, and
/// uncommitted reviews key on the HEAD sha plus a stable literal.
pub fn run_review(
    repo: &Repo,
    out: PipelineOutput,
    review_base: &str,
    head_spec: &str,
) -> anyhow::Result<()> {
    let doc = out
        .document
        .context("invariants failed; nothing to review")?;

    let session = ReviewSession::open(repo, review_base, head_spec, doc, out.view)?;
    let factory = RowFactory::new(repo.clone(), out.base.clone(), out.head.clone());
    let mut app = App::new(session, factory);

    // Terminal guard (vendored, Drop-safe) + chained panic hook.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        vendor::terminal::restore_stdio_best_effort();
        original_hook(info);
    }));
    let mut terminal = vendor::terminal::TerminalFeatures::new()
        .mouse_enabled(false)
        .keyboard_enhancements_supported(false)
        .enter(std::io::stdout())?;

    let mut clipboard: Option<arboard::Clipboard> = arboard::Clipboard::new().ok();
    let mut dirty = true;
    let result = loop {
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
                Effect::Yank(text) => {
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
            break Ok(());
        }
    };

    terminal.restore()?;
    result
}
