//! An open review: the engine-owned session over one plan document.
//!
//! The engine is the backend; renderers are stateless frontends (ADR 0014).
//! A `ReviewSession` owns the store, the document, the diff view and all
//! mutable review state — reviewed marks, findings, resume cursor. Every
//! mutation persists before returning, so a renderer can crash at any point
//! without losing anything, and never touches the store itself.

use std::collections::HashSet;

use crate::schema;

use crate::EngineError;
use crate::forge::{self, OwnComment, Published, RemoteThread};
use crate::model::DiffView;
use crate::plan;
use crate::ports::ReviewStore;
use crate::review_state::{Anchor, Finding, FindingStatus, Lines, ReviewState, Upstream, reanchor};

pub struct ReviewSession<S: ReviewStore> {
    store: S,
    doc: schema::PlanDocument,
    /// Hunk BYTES. Not to be confused with `plan`, which is the document's
    /// arithmetic — see `view()` and `plan()`.
    view: DiffView,
    plan: plan::ReviewView,
    plan_hash: String,
    state: ReviewState,
    findings: Vec<Finding>,
    /// The forge's threads, placed against THIS plan (ADR 0029). A cache of
    /// what was last fetched, never the reader's own work.
    threads: Vec<RemoteThread>,
    /// The login the forge knows the reader as, once told. What makes a
    /// comment with no marker and no record theirs.
    me: Option<String>,
}

impl<S: ReviewStore> ReviewSession<S> {
    /// Open (or resume) the review identified by `(review_base, head_spec)`:
    /// persist the plan, load and re-anchor findings, load state.
    ///
    /// `review_base`/`head_spec` are the review's IDENTITY, not necessarily
    /// the diff endpoints: reviewing uncommitted changes keys on the HEAD sha
    /// plus a stable literal while the synthesized trees churn.
    pub fn open(store: S, doc: schema::PlanDocument, view: DiffView) -> Result<Self, EngineError> {
        let json = doc.to_json()?;
        let plan_hash = plan::plan_hash(&json);
        store.save_plan(&plan_hash, &json)?;
        let mut findings = store.load_findings()?;
        reanchor(&mut findings, &doc, &view, &plan_hash);
        store.save_findings(&findings)?;
        let state = store.load_state()?;
        // Placed afresh on every open: the cache may be from an older plan, and
        // placement is a pure function of the forge's coordinates and this one.
        let mut threads = store.load_threads()?;
        for t in &mut threads {
            forge::place(&doc, &view, t);
        }

        // The projection computes the reviewed-mark keys, so the session no
        // longer derives its own copy of the same arithmetic.
        let plan = plan::ReviewView::project(&doc)?;

        Ok(ReviewSession {
            store,
            doc,
            view,
            plan,
            plan_hash,
            state,
            findings,
            threads,
            me: None,
        })
    }

    // ---------------------------------------------------------------- reads

    pub fn doc(&self) -> &schema::PlanDocument {
        &self.doc
    }

    /// The document's projection: groups, files, counts, dependency edges and
    /// reviewed-mark keys. Renderers read this instead of re-deriving it.
    pub fn plan(&self) -> &plan::ReviewView {
        &self.plan
    }

    pub fn plan_hash(&self) -> &str {
        &self.plan_hash
    }

    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// The forge's review threads as last fetched, placed against this plan.
    pub fn threads(&self) -> &[RemoteThread] {
        &self.threads
    }

    pub fn thread(&self, id: &str) -> Option<&RemoteThread> {
        self.threads.iter().find(|t| t.id == id)
    }

    /// The login the forge knows the reader as, if the forge has been asked.
    pub fn me(&self) -> Option<&str> {
        self.me.as_deref()
    }

    /// Whether `comment` in `thread` is the reader's, and how to act on it:
    /// by a linked record — a marker or a publish's recorded address — or by
    /// author. `None` for anyone else's comment, which is reply-only.
    pub fn own_comment(&self, thread: &str, comment: &str) -> Option<OwnComment> {
        let t = self.thread(thread)?;
        let c = t.comments.iter().find(|c| c.id == comment)?;
        let linked = self.findings.iter().find(|f| {
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

    /// The thread's root as the reader's own comment, if it is theirs.
    pub fn own_root(&self, thread: &str) -> Option<OwnComment> {
        let root = self.thread(thread)?.root()?.id.clone();
        self.own_comment(thread, &root)
    }

    /// A published finding as the comment it became, whether or not its twin
    /// has been fetched back yet.
    pub fn own_of_finding(&self, id: &str) -> Option<OwnComment> {
        let f = self.findings.iter().find(|f| f.id == id)?;
        let up = f.upstream.as_ref()?;
        Some(OwnComment {
            thread: up.thread.clone(),
            comment: up.comment.clone(),
            finding: Some(f.id.clone()),
            body: f.body.clone(),
            at: format!("{}:{}", f.anchor.file, f.anchor.line_span()),
        })
    }

    /// Open findings not yet on the request: what `P` would send and what
    /// `y` copies (ADR 0029).
    pub fn unpublished(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.status == FindingStatus::Open && f.upstream.is_none())
    }

    /// Whether a published finding's fetched twin is present, so the renderer
    /// draws the thread and not the note.
    pub fn is_twinned(&self, finding: &Finding) -> bool {
        finding.upstream.is_some() && self.threads.iter().any(|t| t.is_twin_of(finding))
    }

    /// What a publish would send now, and what it would leave and why.
    pub fn publish_plan(&self, kind: forge::ForgeKind) -> forge::PublishPlan {
        forge::publish_plan(&self.doc, &self.findings, &self.threads, kind.line_rule())
    }

    /// The reviewed-mark key of `hunk` — its exact content digest.
    pub fn hunk_key(&self, hunk: usize) -> &str {
        self.plan.digest(plan::HunkId::from_index(hunk))
    }

    pub fn is_reviewed(&self, hunk_key: &str) -> bool {
        self.state.reviewed_hunks.contains(hunk_key)
    }

    /// Marks that land on a hunk of THIS document.
    ///
    /// Keys from an earlier plan stay on disk and revive if their content
    /// comes back, so counting the stored set would count hunks the reader
    /// cannot see — and could outrun the total the renderer draws it against.
    pub fn reviewed_count(&self) -> usize {
        self.plan
            .count_marked(|digest| self.state.reviewed_hunks.contains(digest))
    }

    /// Canonical hunk indices marked reviewed (owned — safe to hold while
    /// borrowing the session elsewhere).
    pub fn reviewed_hunks(&self) -> HashSet<usize> {
        self.plan
            .hunks_marked(|digest| self.state.reviewed_hunks.contains(digest))
            .into_iter()
            .map(|h| h.index())
            .collect()
    }

    pub fn cursor(&self) -> Option<&(String, usize)> {
        self.state.cursor.as_ref()
    }

    /// The reader's recorded layout choice, or `None` if they have not made
    /// one and the caller should fall back to its configured default.
    pub fn split_diff(&self) -> Option<bool> {
        self.state.split_diff
    }

    pub fn file_view(&self) -> bool {
        self.state.file_view
    }

    /// The reader's recorded wrap choice, or `None` if they have not pressed
    /// `w` on this review.
    pub fn wrap(&self) -> Option<bool> {
        self.state.wrap
    }

    /// The open, unpublished findings as markdown: one `- file:lines: note`
    /// per line.
    ///
    /// The human-readable projection of `findings()`, and domain policy rather
    /// than a renderer's formatting — the reviewer's `y` and `dfr findings
    /// --summary` are the same text, so one cannot drift from the other.
    ///
    /// Deliberately says nothing about groups. A group is how THIS reviewer
    /// chose to read the branch, and the summary is pasted somewhere that has
    /// no idea what `g7` was. A published finding is left out for the same
    /// reason a group is: it is already where it was going (ADR 0029).
    pub fn findings_summary(&self) -> String {
        let mut out = String::new();
        for f in self.unpublished() {
            out.push_str(&format!(
                "- {}:{}: {}\n",
                f.anchor.file,
                f.anchor.line_span(),
                f.body
            ));
        }
        if out.is_empty() {
            out.push_str("(no open findings)\n");
        }
        out
    }

    // ---------------------------- mutations (each persists before returning)

    /// Toggle the reviewed mark of `hunk` itself. Returns the new mark
    /// (true = now reviewed).
    pub fn toggle_reviewed(&mut self, hunk: usize) -> Result<bool, EngineError> {
        let key = self.plan.digest(plan::HunkId::from_index(hunk)).to_string();
        let now = self.state.reviewed_hunks.insert(key.clone());
        if !now {
            self.state.reviewed_hunks.remove(&key);
        }
        self.store.save_state(&self.state)?;
        Ok(now)
    }

    /// Mark a whole set of hunks reviewed (or not) in one write.
    ///
    /// Set semantics, not toggle: a partially reviewed group resolves to the
    /// requested state instead of inverting member by member, and the batch
    /// costs one `save_state` rather than one per hunk.
    pub fn set_reviewed(&mut self, hunk_keys: &[String], on: bool) -> Result<(), EngineError> {
        for key in hunk_keys {
            if on {
                self.state.reviewed_hunks.insert(key.clone());
            } else {
                self.state.reviewed_hunks.remove(key);
            }
        }
        self.store.save_state(&self.state)
    }

    /// Persist the resume position: (group id or file path, row offset).
    pub fn save_cursor(&mut self, id: String, row: usize) -> Result<(), EngineError> {
        self.state.cursor = Some((id, row));
        self.store.save_state(&self.state)
    }

    /// Persist the diff layout (unified / side-by-side).
    pub fn set_split_diff(&mut self, on: bool) -> Result<(), EngineError> {
        self.state.split_diff = Some(on);
        self.store.save_state(&self.state)
    }

    /// Persist the soft-wrap choice.
    pub fn set_wrap(&mut self, on: bool) -> Result<(), EngineError> {
        self.state.wrap = Some(on);
        self.store.save_state(&self.state)
    }

    /// Persist the left-pane view (semantic groups / flat file list).
    pub fn set_file_view(&mut self, on: bool) -> Result<(), EngineError> {
        self.state.file_view = on;
        self.store.save_state(&self.state)
    }

    /// Create a finding on `hunk` and persist it.
    ///
    /// `lines` is what the reviewer pointed at; `None` anchors the hunk's
    /// first changed line, which is what a finding filed from its header
    /// annotates. Either way the anchor is stored as an OFFSET into the hunk,
    /// so it survives the hunk moving in the file (see `Anchor::offset`).
    pub fn add_finding(
        &mut self,
        hunk: usize,
        lines: Option<Lines>,
        body: String,
    ) -> Result<&Finding, EngineError> {
        let h = &self.doc.hunks[hunk];
        let lines = lines.unwrap_or_else(|| {
            let vh = &self.view.hunks[hunk];
            let text = vh
                .added
                .first()
                .or(vh.removed.first())
                .map(|l| String::from_utf8_lossy(l).into_owned())
                .unwrap_or_default();
            let new_side = h.new_count > 0;
            let line = if new_side {
                h.new_start.max(1)
            } else {
                h.old_start.max(1)
            };
            Lines {
                side: if new_side { "new" } else { "old" }.into(),
                start: line,
                end: line,
                start_text: text.clone(),
                end_text: text,
            }
        });
        let old_side = lines.side == "old";
        let (start, count) = if old_side {
            (h.old_start.max(1), h.old_count)
        } else {
            (h.new_start.max(1), h.new_count)
        };
        let end = lines.end.max(lines.start);

        // The re-anchor key comes from the HUNK's own bytes wherever the line
        // is one of its changed lines: `reanchor` matches against those bytes,
        // and a renderer's text has been through tab expansion and trimming on
        // the way to the screen. Outside the changed lines — a context line the
        // reader expanded into view — there is nothing in the hunk to read, so
        // what the renderer saw is what there is.
        let vh = &self.view.hunks[hunk];
        let side_lines = if old_side { &vh.removed } else { &vh.added };
        let raw = |line: u32| -> Option<String> {
            (line >= start && line < start.saturating_add(count))
                .then(|| side_lines.get((line - start) as usize))
                .flatten()
                .map(|l| String::from_utf8_lossy(l).into_owned())
        };
        let line_text = raw(lines.start).unwrap_or(lines.start_text);
        let end_line_text = raw(end).unwrap_or(lines.end_text);

        let finding = Finding::new(
            crate::review_state::now_unix(),
            body,
            self.plan_hash.clone(),
            Anchor {
                file: h.file.clone(),
                side: lines.side,
                line: lines.start,
                end_line: end,
                // Signed, and never clamped: a note on a context line ABOVE
                // the hunk sits at a negative offset, and clamping it to zero
                // silently walked the note down to the hunk's first line on
                // the next regeneration.
                offset: (i64::from(lines.start) - i64::from(start)) as i32,
                span: end - lines.start,
                hunk_digest: h.digest.clone(),
                line_text,
                end_line_text,
            },
        );
        self.findings.push(finding);
        self.store.save_findings(&self.findings)?;
        Ok(self.findings.last().expect("just pushed"))
    }

    /// Rewrite a finding's body in place. Returns whether one was found.
    ///
    /// The id is a handle, not a hash of the text: rewriting a note is not
    /// filing a different one, and the anchor it was written against is the
    /// thing worth keeping. `plan_hash` stays too — the note still describes
    /// the plan it was written on.
    pub fn edit_finding(&mut self, id: &str, body: String) -> Result<bool, EngineError> {
        let Some(f) = self.findings.iter_mut().find(|f| f.id == id) else {
            return Ok(false);
        };
        f.body = body;
        self.store.save_findings(&self.findings)?;
        Ok(true)
    }

    /// Delete a finding by id. Returns whether anything was removed.
    pub fn delete_finding(&mut self, id: &str) -> Result<bool, EngineError> {
        let before = self.findings.len();
        self.findings.retain(|f| f.id != id);
        if self.findings.len() == before {
            return Ok(false);
        }
        self.store.save_findings(&self.findings)?;
        Ok(true)
    }

    /// Draft a reply under a forge thread: a finding that carries the thread's
    /// id and sits where the thread does. Nothing reaches the forge until a
    /// publish sends it (ADR 0029).
    pub fn add_reply(&mut self, thread_id: &str, body: String) -> Result<&Finding, EngineError> {
        let Some(thread) = self.threads.iter().find(|t| t.id == thread_id) else {
            return Err(EngineError::PlanIntegrity(format!(
                "no thread {thread_id} on this review"
            )));
        };
        // A thread nothing in this plan holds has no row, so a reply under it
        // would have none either: filed, listed as open, reachable nowhere.
        // Refused instead; the forge's own page still takes a reply.
        let Some(anchor) = thread.anchor.clone() else {
            return Err(EngineError::PlanIntegrity(format!(
                "thread {thread_id} has no line in this diff; reply on the forge"
            )));
        };
        let mut finding = Finding::new(
            crate::review_state::now_unix(),
            body,
            self.plan_hash.clone(),
            anchor,
        );
        finding.reply_to = Some(thread_id.to_string());
        self.findings.push(finding);
        self.store.save_findings(&self.findings)?;
        Ok(self.findings.last().expect("just pushed"))
    }

    /// Replace the thread cache with a fresh fetch, placed against this plan.
    ///
    /// Then reconcile: a finding with no upstream whose marker a fetched
    /// comment carries IS published, whatever the publish's answer said, and
    /// gets its address now. Returns how many were reconciled. This is what
    /// makes a publish idempotent across a lost answer (ADR 0029).
    ///
    /// When the session knows who the reader is (`set_me`), a comment by that
    /// author with no marker — one sent before markers existed, or written on
    /// the forge's own page — is matched to an unpublished note on the same
    /// file and line with the same text, and the two are linked; a reply the
    /// same way, by thread and text. Weaker than the marker, and enough: the
    /// same author, place and words.
    pub fn set_threads(&mut self, mut threads: Vec<RemoteThread>) -> Result<usize, EngineError> {
        for t in &mut threads {
            forge::place(&self.doc, &self.view, t);
        }
        self.threads = threads;
        let mut reconciled = 0;
        // By marker first: exact.
        for f in self.findings.iter_mut().filter(|f| f.upstream.is_none()) {
            if let Some((t, c)) = self
                .threads
                .iter()
                .find_map(|t| t.published_here(&f.id).map(|c| (t, c)))
            {
                f.upstream = Some(Upstream {
                    thread: t.id.clone(),
                    comment: c.id.clone(),
                });
                reconciled += 1;
            }
        }
        // Then by author, place and words, for comments with no marker.
        if let Some(me) = self.me.as_deref() {
            let same =
                |a: &str, b: &str| a.replace("\r\n", "\n").trim() == b.replace("\r\n", "\n").trim();
            for t in &mut self.threads {
                let Some(anchor) = t.anchor.clone() else {
                    continue;
                };
                for (i, c) in t.comments.iter_mut().enumerate() {
                    if c.finding.is_some() || c.author != me {
                        continue;
                    }
                    let hit = self.findings.iter_mut().find(|f| {
                        f.upstream.is_none()
                            && same(&f.body, &c.body)
                            && if i == 0 {
                                f.reply_to.is_none()
                                    && f.anchor.file == anchor.file
                                    && f.anchor.side == anchor.side
                                    && f.anchor.end_line.max(f.anchor.line)
                                        == anchor.end_line.max(anchor.line)
                            } else {
                                f.reply_to.as_deref() == Some(t.id.as_str())
                            }
                    });
                    if let Some(f) = hit {
                        f.upstream = Some(Upstream {
                            thread: t.id.clone(),
                            comment: c.id.clone(),
                        });
                        c.finding = Some(f.id.clone());
                        reconciled += 1;
                    }
                }
            }
        }
        self.store.save_threads(&self.threads)?;
        if reconciled > 0 {
            self.store.save_findings(&self.findings)?;
        }
        Ok(reconciled)
    }

    /// Tell the session who the reader is on the forge. Asked of the forge
    /// once per sitting; a comment by this author is the reader's own.
    pub fn set_me(&mut self, login: String) {
        self.me = Some(login);
    }

    /// Mirror a resolve the forge has already accepted. Returns whether the
    /// thread was known.
    pub fn set_thread_resolved(&mut self, id: &str, resolved: bool) -> Result<bool, EngineError> {
        let Some(t) = self.threads.iter_mut().find(|t| t.id == id) else {
            return Ok(false);
        };
        t.resolved = resolved;
        self.store.save_threads(&self.threads)?;
        Ok(true)
    }

    /// A comment of the reader's, rewritten: the forge has already taken the
    /// new body, so the cached thread follows it, and the record too when a
    /// finding is linked to the comment — fetched twin or not. Returns whether
    /// anything was known.
    pub fn edit_comment(
        &mut self,
        thread: &str,
        comment: &str,
        body: String,
    ) -> Result<bool, EngineError> {
        // The cached comment, when the twin has been fetched.
        let mut linked = None;
        let mut known = false;
        if let Some(c) = self
            .threads
            .iter_mut()
            .find(|t| t.id == thread)
            .and_then(|t| t.comments.iter_mut().find(|c| c.id == comment))
        {
            c.body = body.clone();
            linked = c.finding.clone();
            known = true;
            self.store.save_threads(&self.threads)?;
        }
        // The record, when one is linked — fetched twin or not.
        if let Some(f) = self.findings.iter_mut().find(|f| {
            linked.as_deref() == Some(f.id.as_str())
                || f.upstream.as_ref().is_some_and(|u| u.comment == comment)
        }) {
            f.body = body;
            known = true;
            self.store.save_findings(&self.findings)?;
        }
        Ok(known)
    }

    /// A comment of the reader's the forge has already deleted: drop it from
    /// the cache, the thread with it when nothing is left, and the linked
    /// finding's record — fetched twin or not. Returns whether anything was
    /// known.
    pub fn delete_comment(&mut self, thread: &str, comment: &str) -> Result<bool, EngineError> {
        let mut linked = None;
        let mut known = false;
        if let Some(t) = self.threads.iter_mut().find(|t| t.id == thread)
            && let Some(pos) = t.comments.iter().position(|c| c.id == comment)
        {
            linked = t.comments.remove(pos).finding;
            known = true;
            self.threads.retain(|t| !t.comments.is_empty());
            self.store.save_threads(&self.threads)?;
        }
        let before = self.findings.len();
        self.findings.retain(|f| {
            linked.as_deref() != Some(f.id.as_str())
                && f.upstream.as_ref().is_none_or(|u| u.comment != comment)
        });
        if self.findings.len() != before {
            known = true;
            self.store.save_findings(&self.findings)?;
        }
        Ok(known)
    }

    /// Record where a publish put each finding, so the next publish sends
    /// only what is new and the renderer can hide each behind its twin.
    pub fn mark_published(&mut self, published: &[Published]) -> Result<usize, EngineError> {
        let mut n = 0;
        for p in published {
            if let Some(f) = self.findings.iter_mut().find(|f| f.id == p.finding) {
                f.upstream = Some(Upstream {
                    thread: p.thread.clone(),
                    comment: p.comment.clone(),
                });
                n += 1;
            }
        }
        if n > 0 {
            self.store.save_findings(&self.findings)?;
        }
        Ok(n)
    }

    /// Delete every finding not on the request. Returns how many went.
    ///
    /// A published finding is kept: its record is what lets the reader edit or
    /// delete the comment on the forge, and what stops the next publish from
    /// sending it again (ADR 0029). Deleting one is `dd`, which asks.
    ///
    /// One write, not one per note: the store rewrites the whole file on every
    /// save, so a loop over `delete_finding` would rewrite it N times to reach
    /// the same file.
    pub fn clear_findings(&mut self) -> Result<usize, EngineError> {
        let before = self.findings.len();
        self.findings.retain(|f| f.upstream.is_some());
        let n = before - self.findings.len();
        if n == 0 {
            return Ok(0);
        }
        self.store.save_findings(&self.findings)?;
        Ok(n)
    }
}
