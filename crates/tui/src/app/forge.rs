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

/// A comment on the forge that is the reader's: by author, by marker, or by
/// the address a publish recorded. What `c` edits and `dd` deletes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnComment {
    pub thread: String,
    pub comment: String,
    /// The local record, when one is linked.
    pub finding: Option<String>,
    pub body: String,
    /// `file:lines`, for the prompt.
    pub at: String,
}

/// A fetch's answer: the threads, and the reader's login when it was asked
/// for this time.
type Fetched = (Result<Vec<RemoteThread>, ForgeError>, Option<String>);

/// A forge call whose answer has not come back yet.
pub(super) enum Inflight {
    Fetch(Receiver<Fetched>),
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
    Edit {
        own: OwnComment,
        body: String,
        rx: Receiver<Result<(), ForgeError>>,
    },
    Delete {
        own: OwnComment,
        rx: Receiver<Result<(), ForgeError>>,
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
        // Who the reader is, asked once: the answer does not change while
        // the reviewer is open, and it is what makes a comment theirs.
        let ask_me = self.me.is_none();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let me = ask_me.then(|| forge.whoami().ok()).flatten();
            let _ = tx.send((forge.threads(&req), me));
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
                Ok((result, me)) => Answer::Fetched(result, me),
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
            Inflight::Edit { own, body, rx } => match rx.try_recv() {
                Ok(result) => Answer::Edited(own.clone(), body.clone(), result),
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => Answer::Lost,
            },
            Inflight::Delete { own, rx } => match rx.try_recv() {
                Ok(result) => Answer::Deleted(own.clone(), result),
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => Answer::Lost,
            },
        };
        self.inflight = None;
        match answer {
            Answer::Fetched(Ok(threads), me) => {
                if me.is_some() {
                    self.me = me;
                }
                let n = threads.len();
                match self.session.set_threads(threads, self.me.as_deref()) {
                    Ok(reconciled) => {
                        let unplaced = self
                            .session
                            .threads()
                            .iter()
                            .filter(|t| t.anchor.is_none())
                            .count();
                        let mut status = match (n, unplaced) {
                            (0, _) => "no review threads on the request".to_string(),
                            (n, 0) => format!("{n} review thread{}", plural(n)),
                            (n, u) => format!(
                                "{n} review thread{} · {u} with no line in this diff",
                                plural(n)
                            ),
                        };
                        // A note the forge already had, found by its marker:
                        // it is published now whatever the last publish said.
                        if reconciled > 0 {
                            status.push_str(&format!(
                                " · {reconciled} finding{} found already published",
                                plural(reconciled)
                            ));
                        }
                        self.status = status;
                    }
                    Err(e) => self.status = format!("save failed: {e:#}"),
                }
                self.rebuild_rows();
            }
            Answer::Fetched(Err(e), _) => {
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
                // What the publish's answer named, then what the refetched
                // threads carry by marker: a finding is published when either
                // says so, and the count is read from the findings afterwards
                // rather than from the answer alone.
                let marked = self.session.mark_published(&outcome.published);
                let cached = self
                    .session
                    .set_threads(outcome.threads, self.me.as_deref());
                let landed = self
                    .session
                    .findings()
                    .iter()
                    .filter(|f| f.upstream.is_some())
                    .count()
                    .min(sent);
                self.status = match (marked, cached) {
                    (Err(e), _) | (_, Err(e)) => format!("save failed: {e:#}"),
                    _ if landed < sent => format!(
                        "published {landed} of {sent} · {} not confirmed by the forge, R to check, P to retry",
                        sent - landed
                    ),
                    _ => format!("published {landed} comment{}", plural(landed)),
                };
                self.rebuild_rows();
            }
            Answer::Published(_, Err(e)) => {
                self.status = format!("nothing published: {e}");
            }
            Answer::Edited(own, body, Ok(())) => {
                match self.session.edit_comment(&own.thread, &own.comment, body) {
                    Ok(true) => self.status = "comment rewritten on the request".into(),
                    Ok(false) => self.status = "that comment is gone".into(),
                    Err(e) => self.status = format!("save failed: {e:#}"),
                }
                self.rebuild_rows();
            }
            Answer::Edited(_, _, Err(e)) => {
                self.status = format!("the comment was not changed: {e}");
            }
            Answer::Deleted(own, Ok(())) => {
                match self.session.delete_comment(&own.thread, &own.comment) {
                    Ok(true) => self.status = "comment deleted on the request".into(),
                    Ok(false) => self.status = "that comment is gone".into(),
                    Err(e) => self.status = format!("save failed: {e:#}"),
                }
                self.rebuild_rows();
                self.reopen_findings();
            }
            Answer::Deleted(_, Err(e)) => {
                self.status = format!("the comment was not deleted: {e}");
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
            Some(RowKind::Thread { thread, .. }) => self.session.thread(thread),
            _ => None,
        }
    }

    /// The comment of the reader's the cursor is in, if any: a thread row of
    /// a comment they wrote — by author, by marker, or by a publish's recorded
    /// address — or the row of a published note whose twin is not fetched
    /// yet. `None` on anyone else's comment.
    pub(super) fn own_comment_at_cursor(&self) -> Option<OwnComment> {
        match self.rows.get(self.cursor).map(|r| &r.kind) {
            Some(RowKind::Thread {
                thread, comment, ..
            }) => self.own_comment(thread, comment),
            Some(RowKind::Finding(id, _)) => {
                let f = self
                    .session
                    .findings()
                    .iter()
                    .find(|f| &f.id == id && f.upstream.is_some())?;
                let up = f.upstream.as_ref()?;
                Some(OwnComment {
                    thread: up.thread.clone(),
                    comment: up.comment.clone(),
                    finding: Some(f.id.clone()),
                    body: f.body.clone(),
                    at: format!("{}:{}", f.anchor.file, f.anchor.line_span()),
                })
            }
            _ => None,
        }
    }

    /// The thread's root as the reader's own comment, if it is theirs.
    pub(super) fn own_root(&self, thread: &str) -> Option<OwnComment> {
        let root = self.session.thread(thread)?.root()?.id.clone();
        self.own_comment(thread, &root)
    }

    fn own_comment(&self, thread: &str, comment: &str) -> Option<OwnComment> {
        let t = self.session.thread(thread)?;
        let c = t.comments.iter().find(|c| c.id == comment)?;
        let linked = self.session.findings().iter().find(|f| {
            c.finding.as_deref() == Some(f.id.as_str())
                || f.upstream.as_ref().is_some_and(|u| u.comment == c.id)
        });
        let mine = linked.is_some() || self.me.as_deref() == Some(c.author.as_str());
        if !mine {
            return None;
        }
        let at = match &t.anchor {
            Some(a) => format!("{}:{}", a.file, a.line_span()),
            None => t.path.clone(),
        };
        Some(OwnComment {
            thread: thread.to_string(),
            comment: comment.to_string(),
            finding: linked.map(|f| f.id.clone()),
            body: c.body.clone(),
            at,
        })
    }

    /// Rewrite a comment of the reader's: on the forge first, and the cache
    /// and record follow when the forge has answered. A linked finding sends
    /// its marker with the new body, so a comment healed by author carries
    /// one from here on.
    pub(super) fn start_edit_comment(&mut self, own: OwnComment, body: String) {
        let Some(link) = &self.forge else {
            self.status = "this review is not of a pull request".into();
            return;
        };
        if self.inflight.is_some() {
            self.status = "still syncing with the forge".into();
            return;
        }
        let (forge, req) = (Arc::clone(&link.forge), link.request.clone());
        let sent = match &own.finding {
            Some(id) => forge::with_marker(&body, id),
            None => body.clone(),
        };
        let (thread, comment) = (own.thread.clone(), own.comment.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(forge.edit_comment(&req, &thread, &comment, &sent));
        });
        self.inflight = Some(Inflight::Edit { own, body, rx });
        self.status = "rewriting the comment on the request…".into();
    }

    /// Delete a comment of the reader's: on the forge first.
    pub(super) fn start_delete_comment(&mut self, own: OwnComment) {
        let Some(link) = &self.forge else {
            self.status = "this review is not of a pull request".into();
            return;
        };
        if self.inflight.is_some() {
            self.status = "still syncing with the forge".into();
            return;
        }
        let (forge, req) = (Arc::clone(&link.forge), link.request.clone());
        let (thread, comment) = (own.thread.clone(), own.comment.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(forge.delete_comment(&req, &thread, &comment));
        });
        self.inflight = Some(Inflight::Delete { own, rx });
        self.status = "deleting the comment on the request…".into();
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
    Fetched(Result<Vec<RemoteThread>, ForgeError>, Option<String>),
    Resolved(String, bool, Result<(), ForgeError>),
    Published(usize, Result<PublishOutcome, ForgeError>),
    Edited(OwnComment, String, Result<(), ForgeError>),
    Deleted(OwnComment, Result<(), ForgeError>),
    Lost,
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
