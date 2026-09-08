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

use differential_engine::forge::{
    self, Forge, ForgeError, OwnComment, PublishOutcome, RemoteThread, Request,
};

use crate::rows::RowKind;

use super::*;

/// The forge a review is of, as the application layer composed it.
pub struct ForgeLink {
    pub forge: Arc<dyn Forge>,
    pub request: Request,
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
        /// The findings the batch carried, so the answer counts what landed
        /// of THIS batch and not of every publish before it.
        sent: Vec<String>,
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

    /// The forge and the request, when a call may go out: this review is of
    /// a request, and no call is already in flight. Otherwise the footer says
    /// which, and the caller returns. Every starter below opens with this.
    fn ready(&mut self) -> Option<(Arc<dyn Forge>, Request)> {
        let Some(link) = &self.forge else {
            self.status = "this review is not of a pull request".into();
            return None;
        };
        if self.inflight.is_some() {
            self.status = "still syncing with the forge".into();
            return None;
        }
        Some((Arc::clone(&link.forge), link.request.clone()))
    }

    /// Fetch the request's review threads on a worker thread.
    pub fn start_fetch(&mut self) {
        let Some((forge, req)) = self.ready() else {
            return;
        };
        // Who the reader is, asked once: the answer does not change while
        // the reviewer is open, and it is what makes a comment theirs.
        let ask_me = self.session.me().is_none();
        self.inflight = Some(Inflight::Fetch(spawn(move || {
            let me = ask_me.then(|| forge.whoami().ok()).flatten();
            (forge.threads(&req), me)
        })));
    }

    /// Take a finished forge call, if one has finished. `true` when the
    /// screen changed.
    pub fn poll_forge(&mut self) -> bool {
        // Taken, then put back if the answer is not in yet: owning the value
        // lets each arm move its fields into the answer instead of cloning
        // them to drop the original a line later.
        let Some(inflight) = self.inflight.take() else {
            return false;
        };
        let (answer, waiting) = match inflight {
            Inflight::Fetch(rx) => match rx.try_recv() {
                Ok((result, me)) => (Answer::Fetched(result, me), None),
                Err(TryRecvError::Empty) => (Answer::Lost, Some(Inflight::Fetch(rx))),
                Err(TryRecvError::Disconnected) => (Answer::Lost, None),
            },
            Inflight::Resolve {
                thread,
                resolved,
                rx,
            } => match rx.try_recv() {
                Ok(result) => (Answer::Resolved(thread, resolved, result), None),
                Err(TryRecvError::Empty) => (
                    Answer::Lost,
                    Some(Inflight::Resolve {
                        thread,
                        resolved,
                        rx,
                    }),
                ),
                Err(TryRecvError::Disconnected) => (Answer::Lost, None),
            },
            Inflight::Publish { sent, rx } => match rx.try_recv() {
                Ok(result) => (Answer::Published(sent, result), None),
                Err(TryRecvError::Empty) => (Answer::Lost, Some(Inflight::Publish { sent, rx })),
                Err(TryRecvError::Disconnected) => (Answer::Lost, None),
            },
            Inflight::Edit { own, body, rx } => match rx.try_recv() {
                Ok(result) => (Answer::Edited(own, body, result), None),
                Err(TryRecvError::Empty) => (Answer::Lost, Some(Inflight::Edit { own, body, rx })),
                Err(TryRecvError::Disconnected) => (Answer::Lost, None),
            },
            Inflight::Delete { own, rx } => match rx.try_recv() {
                Ok(result) => (Answer::Deleted(own, result), None),
                Err(TryRecvError::Empty) => (Answer::Lost, Some(Inflight::Delete { own, rx })),
                Err(TryRecvError::Disconnected) => (Answer::Lost, None),
            },
        };
        if let Some(still) = waiting {
            self.inflight = Some(still);
            return false;
        }
        match answer {
            Answer::Fetched(Ok(threads), me) => {
                if let Some(me) = me {
                    self.session.set_me(me);
                }
                let n = threads.len();
                match self.session.set_threads(threads) {
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
                self.notice("could not fetch review threads", &e);
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
                self.notice("the thread was not resolved", &e);
            }
            Answer::Published(sent, Ok(outcome)) => {
                // What the publish's answer named, then what the refetched
                // threads carry by marker: a finding is published when either
                // says so, and the count is read from THIS batch's findings
                // afterwards rather than from the answer alone. A refetch that
                // failed is said; the comments are on the request regardless.
                let marked = self.session.mark_published(&outcome.published);
                let refetch = match outcome.threads {
                    Ok(threads) => self.session.set_threads(threads).map(|_| None),
                    Err(e) => Ok(Some(e)),
                };
                let landed = sent
                    .iter()
                    .filter(|id| {
                        self.session
                            .findings()
                            .iter()
                            .any(|f| &f.id == *id && f.upstream.is_some())
                    })
                    .count();
                let total = sent.len();
                self.status = match (marked, refetch) {
                    (Err(e), _) | (_, Err(e)) => format!("save failed: {e:#}"),
                    // The forge took part of the batch and then stopped. What
                    // it took is recorded; the rest is still the reader's, and
                    // the next P sends only that.
                    _ if outcome.failed.is_some() => {
                        self.notice(
                            "the forge stopped part-way",
                            outcome.failed.as_ref().expect("guarded"),
                        );
                        format!(
                            "published {landed} of {total} · the forge stopped part-way · R to check, P to send the rest"
                        )
                    }
                    (_, Ok(Some(e))) => format!(
                        "published {landed} of {total} · the threads could not be fetched back ({e}) · R to retry"
                    ),
                    _ if landed < total => format!(
                        "published {landed} of {total} · {} not confirmed by the forge, R to check, P to retry",
                        total - landed
                    ),
                    _ => format!("published {landed} comment{}", plural(landed)),
                };
                self.rebuild_rows();
            }
            Answer::Published(_, Err(e)) => {
                self.notice("nothing published", &e);
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
                self.notice("the comment was not changed", &e);
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
                self.notice("the comment was not deleted", &e);
            }
            Answer::Lost => self.status = "the forge call was lost".into(),
        }
        true
    }

    /// `P`: show what would go and what would stay, and wait for `y`.
    pub(super) fn offer_publish(&mut self) {
        let Some((_, req)) = self.ready() else {
            return;
        };
        let plan = self.session.publish_plan(req.kind);
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
        let Some((forge, req)) = self.ready() else {
            return;
        };
        let head = self.session.doc().source.head.clone();
        let sent: Vec<String> = plan
            .batch
            .comments
            .iter()
            .map(|c| c.finding.clone())
            .chain(plan.batch.replies.iter().map(|r| r.finding.clone()))
            .collect();
        let n = sent.len();
        let rx = spawn(move || forge::publish(forge.as_ref(), &req, &head, &plan.batch));
        self.inflight = Some(Inflight::Publish { sent, rx });
        self.status = format!("publishing {n} comment{}…", plural(n));
    }

    /// Show a forge failure in full. The footer holds one line and cuts the
    /// rest, and the rest — the exit code, the forge's own words — is what a
    /// reader needs to know what to do. The footer keeps the short form.
    fn notice(&mut self, title: &str, error: &dyn std::fmt::Display) {
        let text = error.to_string();
        self.status = format!("{title} · the details are on screen");
        self.mode = Mode::Notice {
            title: title.to_string(),
            text,
        };
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
            }) => self.session.own_comment(thread, comment),
            Some(RowKind::Finding(id, _)) => self.session.own_of_finding(id),
            _ => None,
        }
    }

    /// Rewrite a comment of the reader's: on the forge first, and the cache
    /// and record follow when the forge has answered. A linked finding sends
    /// its marker with the new body, so a comment healed by author carries
    /// one from here on.
    pub(super) fn start_edit_comment(&mut self, own: OwnComment, body: String) {
        let Some((forge, req)) = self.ready() else {
            return;
        };
        let sent = match &own.finding {
            Some(id) => forge::with_marker(&body, id),
            None => body.clone(),
        };
        let (thread, comment) = (own.thread.clone(), own.comment.clone());
        let rx = spawn(move || forge.edit_comment(&req, &thread, &comment, &sent));
        self.inflight = Some(Inflight::Edit { own, body, rx });
        self.status = "rewriting the comment on the request…".into();
    }

    /// Delete a comment of the reader's: on the forge first.
    pub(super) fn start_delete_comment(&mut self, own: OwnComment) {
        let Some((forge, req)) = self.ready() else {
            return;
        };
        let (thread, comment) = (own.thread.clone(), own.comment.clone());
        let rx = spawn(move || forge.delete_comment(&req, &thread, &comment));
        self.inflight = Some(Inflight::Delete { own, rx });
        self.status = "deleting the comment on the request…".into();
    }

    /// `x`: flip the thread under the cursor on the forge. The forge answers
    /// on a worker thread; the local copy changes when it has.
    /// Open or close the resolved thread under the cursor. A resolved thread
    /// is collapsed to its header by default; this is a local reading toggle,
    /// nothing reaches the forge.
    pub(super) fn toggle_thread_expanded(&mut self) {
        let Some(RowKind::Thread { thread, .. }) =
            self.rows.get(self.cursor).map(|r| r.kind.clone())
        else {
            return;
        };
        if self.expanded_threads.remove(&thread) {
            self.status = "thread collapsed".into();
        } else {
            self.expanded_threads.insert(thread);
            self.status = "thread expanded".into();
        }
        self.rebuild_rows();
    }

    pub(super) fn toggle_thread_resolved(&mut self) {
        let Some(t) = self.thread_at_cursor() else {
            self.status = "x resolves the review thread under the cursor".into();
            return;
        };
        let (id, resolved) = (t.id.clone(), !t.resolved);
        let Some((forge, req)) = self.ready() else {
            return;
        };
        let thread = id.clone();
        let rx = spawn(move || forge.set_resolved(&req, &thread, resolved));
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
    Published(Vec<String>, Result<PublishOutcome, ForgeError>),
    Edited(OwnComment, String, Result<(), ForgeError>),
    Deleted(OwnComment, Result<(), ForgeError>),
    Lost,
}

/// Run one forge call on a worker thread and keep the receiving end. The
/// loop asks `poll_forge` whether the answer has come.
fn spawn<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(job());
    });
    rx
}
