//! The forge, from the reviewer's side (ADR 0029, `spec/forge.md`).
//!
//! Every call to the forge is a subprocess that takes a second or more, and
//! the reviewer's loop draws nothing while a key handler runs. So no handler
//! calls the forge. It starts a worker thread, keeps the receiving end, and
//! the loop asks `poll_forge` on every turn whether the answer has arrived —
//! the same shape the splash uses for the pipeline, one call at a time.
//!
//! One call in flight at once. A second request while one is out is refused
//! with a message rather than queued: the reader can see the `syncing` pill
//! and press again, and a queue is state that has to be explained.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};

use differential_engine::forge::{self, Forge, ForgeError, PublishOutcome, RemoteThread, Request};

use crate::rows::RowKind;

use super::*;

/// The forge a review is of, as the application layer composed it.
pub struct ForgeLink {
    pub forge: Arc<dyn Forge>,
    pub request: Request,
}

/// A forge call whose answer has not come back yet.
pub(super) enum Inflight {
    Fetch(Receiver<Result<Vec<RemoteThread>, ForgeError>>),
    Resolve {
        thread: String,
        resolved: bool,
        rx: Receiver<Result<(), ForgeError>>,
    },
    Publish {
        /// How many the batch carried, so the answer can say what the forge
        /// did not confirm.
        sent: usize,
        rx: Receiver<Result<PublishOutcome, ForgeError>>,
    },
}

impl App {
    /// Attach the forge. Nothing is fetched until `start_fetch`.
    pub fn link_forge(&mut self, link: ForgeLink) {
        self.forge = Some(link);
    }

    /// Whether a forge call is out. The footer wears a pill while it is.
    pub fn syncing(&self) -> bool {
        self.inflight.is_some()
    }

    /// Whether this review is of a request at all.
    pub fn has_forge(&self) -> bool {
        self.forge.is_some()
    }

    /// Fetch the request's review threads on a worker thread.
    pub fn start_fetch(&mut self) {
        let Some(link) = &self.forge else {
            self.status = "this review is not of a pull request".into();
            return;
        };
        if self.inflight.is_some() {
            self.status = "still syncing with the forge".into();
            return;
        }
        let (forge, req) = (Arc::clone(&link.forge), link.request.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(forge.threads(&req));
        });
        self.inflight = Some(Inflight::Fetch(rx));
    }

    /// Take a finished forge call, if one has finished. `true` when the
    /// screen changed.
    pub fn poll_forge(&mut self) -> bool {
        let Some(inflight) = &self.inflight else {
            return false;
        };
        let answer = match inflight {
            Inflight::Fetch(rx) => match rx.try_recv() {
                Ok(result) => Answer::Fetched(result),
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => Answer::Lost,
            },
            Inflight::Resolve {
                thread,
                resolved,
                rx,
            } => match rx.try_recv() {
                Ok(result) => Answer::Resolved(thread.clone(), *resolved, result),
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => Answer::Lost,
            },
            Inflight::Publish { sent, rx } => match rx.try_recv() {
                Ok(result) => Answer::Published(*sent, result),
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => Answer::Lost,
            },
        };
        self.inflight = None;
        match answer {
            Answer::Fetched(Ok(threads)) => {
                let n = threads.len();
                match self.session.set_threads(threads) {
                    Ok(()) => {
                        let unplaced = self
                            .session
                            .threads()
                            .iter()
                            .filter(|t| t.anchor.is_none())
                            .count();
                        self.status = match (n, unplaced) {
                            (0, _) => "no review threads on the request".into(),
                            (n, 0) => format!("{n} review thread{}", plural(n)),
                            (n, u) => format!(
                                "{n} review thread{} · {u} with no line in this diff",
                                plural(n)
                            ),
                        };
                    }
                    Err(e) => self.status = format!("save failed: {e:#}"),
                }
                self.rebuild_rows();
            }
            Answer::Fetched(Err(e)) => {
                // The cache stands: the reader keeps what was fetched last time
                // and is told why it is not fresher.
                self.status = format!("could not fetch review threads: {e}");
            }
            Answer::Resolved(thread, resolved, Ok(())) => {
                match self.session.set_thread_resolved(&thread, resolved) {
                    Ok(true) => {
                        self.status = if resolved {
                            "thread resolved".into()
                        } else {
                            "thread reopened".into()
                        }
                    }
                    Ok(false) => self.status = "that thread is gone".into(),
                    Err(e) => self.status = format!("save failed: {e:#}"),
                }
                self.rebuild_rows();
            }
            Answer::Resolved(_, _, Err(e)) => {
                self.status = format!("could not resolve the thread: {e}");
            }
            Answer::Published(sent, Ok(outcome)) => {
                let landed = outcome.published.len();
                let marked = self.session.mark_published(&outcome.published);
                let cached = self.session.set_threads(outcome.threads);
                self.status = match (marked, cached) {
                    (Err(e), _) | (_, Err(e)) => format!("save failed: {e:#}"),
                    _ if landed < sent => format!(
                        "published {landed} of {sent} · {} not confirmed by the forge, P again to retry",
                        sent - landed
                    ),
                    _ => format!("published {landed} comment{}", plural(landed)),
                };
                self.rebuild_rows();
            }
            Answer::Published(_, Err(e)) => {
                self.status = format!("nothing published: {e}");
            }
            Answer::Lost => self.status = "the forge call was lost".into(),
        }
        true
    }

    /// `P`: show what would go and what would stay, and wait for `y`.
    pub(super) fn offer_publish(&mut self) {
        if self.forge.is_none() {
            self.status = "this review is not of a pull request".into();
            return;
        }
        if self.inflight.is_some() {
            self.status = "still syncing with the forge".into();
            return;
        }
        let plan = self.session.publish_plan();
        if plan.batch.is_empty() {
            self.status = match plan.excluded.len() {
                0 => "nothing to publish: every open finding is on the request".into(),
                n => format!(
                    "nothing to publish · {n} finding{} the request's diff cannot hold",
                    plural(n)
                ),
            };
            return;
        }
        self.mode = Mode::Publish { plan };
    }

    /// `y` in the publish modal: send the batch on a worker thread.
    pub(super) fn start_publish(&mut self, plan: forge::PublishPlan) {
        let Some(link) = &self.forge else {
            return;
        };
        let (forge, req) = (Arc::clone(&link.forge), link.request.clone());
        let head = self.session.doc().source.head.clone();
        let sent = plan.batch.len();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(forge::publish(forge.as_ref(), &req, &head, &plan.batch));
        });
        self.inflight = Some(Inflight::Publish { sent, rx });
        self.status = format!("publishing {sent} comment{}…", plural(sent));
    }

    /// The thread whose rows the cursor is in, if any.
    pub(super) fn thread_at_cursor(&self) -> Option<&RemoteThread> {
        match self.rows.get(self.cursor).map(|r| &r.kind) {
            Some(RowKind::Thread(id, _)) => self.session.thread(id),
            _ => None,
        }
    }

    /// `x`: flip the thread under the cursor on the forge. The forge answers
    /// on a worker thread; the local copy changes when it has.
    pub(super) fn toggle_thread_resolved(&mut self) {
        let Some(t) = self.thread_at_cursor() else {
            self.status = "x resolves the review thread under the cursor".into();
            return;
        };
        let (id, resolved) = (t.id.clone(), !t.resolved);
        let Some(link) = &self.forge else {
            self.status = "this review is not of a pull request".into();
            return;
        };
        if self.inflight.is_some() {
            self.status = "still syncing with the forge".into();
            return;
        }
        let (forge, req) = (Arc::clone(&link.forge), link.request.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = id.clone();
        std::thread::spawn(move || {
            let _ = tx.send(forge.set_resolved(&req, &thread, resolved));
        });
        self.inflight = Some(Inflight::Resolve {
            thread: id,
            resolved,
            rx,
        });
    }

    /// Save a reply drafted under a thread. Local until a publish sends it.
    pub(super) fn add_reply(&mut self, thread: &str, body: String) {
        match self.session.add_reply(thread, body) {
            Ok(_) => self.status = "reply saved · P publishes".into(),
            Err(e) => self.status = format!("save failed: {e:#}"),
        }
        self.rebuild_rows();
    }
}

enum Answer {
    Fetched(Result<Vec<RemoteThread>, ForgeError>),
    Resolved(String, bool, Result<(), ForgeError>),
    Published(usize, Result<PublishOutcome, ForgeError>),
    Lost,
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
