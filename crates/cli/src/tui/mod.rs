//! `dfr review` — the terminal reviewer over a grouped, ordered document.

pub mod app;
pub mod rows;
pub mod theme;
pub mod vendor;

use std::time::Duration;

use anyhow::Context;
use crossterm::event::{self, Event};
use differential_engine::PipelineOutput;
use differential_engine::gitio::Repo;
use differential_engine::review_state::{ReviewStore, reanchor};

use app::{App, Effect};
use rows::RowFactory;

/// Open the reviewer. `head_spec` is the head AS TYPED (branch names keep a
/// review's identity while the tip moves).
pub fn run_review(repo: &Repo, out: PipelineOutput, head_spec: &str) -> anyhow::Result<()> {
    let doc = out
        .document
        .context("invariants failed; nothing to review")?;

    let store = ReviewStore::open(repo, &out.base, head_spec)?;
    let plan_hash = store.save_plan(&doc)?;
    let mut findings = store.load_findings()?;
    reanchor(&mut findings, &doc, &out.view, &plan_hash);
    store.save_findings(&findings)?;
    let state = store.load_state()?;

    let factory = RowFactory::new(repo.clone(), out.base.clone(), out.head.clone());
    let mut app = App::new(doc, out.view, plan_hash, factory, state, findings);

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
                Effect::SaveState => {
                    app.state.cursor = Some((
                        app.groups
                            .get(app.selected_group)
                            .map(|g| g.id.clone())
                            .unwrap_or_default(),
                        app.cursor,
                    ));
                    store.save_state(&app.state)?;
                }
                Effect::SaveFindings => store.save_findings(&app.findings)?,
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
