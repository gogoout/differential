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
use crate::model::DiffView;
use crate::plan;
use crate::ports::ReviewStore;
use crate::review_state::{Anchor, Finding, FindingStatus, Lines, ReviewState, reanchor};

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
        self.reviewed_hunks().len()
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

    /// The open findings as markdown: one `- file:lines: note` per line.
    ///
    /// The human-readable projection of `findings()`, and domain policy rather
    /// than a renderer's formatting — the reviewer's `y` and `dfr findings
    /// --summary` are the same text, so one cannot drift from the other.
    ///
    /// Deliberately says nothing about groups. A group is how THIS reviewer
    /// chose to read the branch, and the summary is pasted somewhere that has
    /// no idea what `g7` was.
    pub fn findings_summary(&self) -> String {
        let mut out = String::new();
        for f in self
            .findings
            .iter()
            .filter(|f| f.status == FindingStatus::Open)
        {
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

    /// Delete every finding. Returns how many there were.
    ///
    /// One write, not one per note: the store rewrites the whole file on every
    /// save, so a loop over `delete_finding` would rewrite it N times to reach
    /// the same empty file.
    pub fn clear_findings(&mut self) -> Result<usize, EngineError> {
        let n = self.findings.len();
        if n == 0 {
            return Ok(0);
        }
        self.findings.clear();
        self.store.save_findings(&self.findings)?;
        Ok(n)
    }
}
