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
use crate::review_state::{Anchor, Finding, ReviewState, reanchor};

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

    /// The canonical diff's CONTENT — hunk bytes. `plan()` is the document's
    /// arithmetic; the two are one letter apart in the type names and quite
    /// different things.
    pub fn view(&self) -> &DiffView {
        &self.view
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

    /// Content key of the class owning `hunk`.
    pub fn hunk_class_key(&self, hunk: usize) -> &str {
        self.plan.hunk_key(plan::HunkId::from_index(hunk))
    }

    /// Content key for a class id (present for every class in the document).
    pub fn class_key(&self, class_id: &str) -> &str {
        self.plan.class_key(class_id)
    }

    pub fn is_reviewed(&self, class_key: &str) -> bool {
        self.state.reviewed_classes.contains(class_key)
    }

    pub fn reviewed_count(&self) -> usize {
        self.state.reviewed_classes.len()
    }

    /// Canonical hunk indices whose class is marked reviewed (owned — safe to
    /// hold while borrowing the session elsewhere).
    pub fn reviewed_hunks(&self) -> HashSet<usize> {
        self.plan
            .hunks_with_keys(|key| self.state.reviewed_classes.contains(key))
            .into_iter()
            .map(|h| h.index())
            .collect()
    }

    pub fn cursor(&self) -> Option<&(String, usize)> {
        self.state.cursor.as_ref()
    }

    pub fn split_diff(&self) -> bool {
        self.state.split_diff
    }

    pub fn file_view(&self) -> bool {
        self.state.file_view
    }

    // ---------------------------- mutations (each persists before returning)

    /// Toggle the reviewed mark of the class owning `hunk`. Returns the new
    /// mark (true = now reviewed).
    pub fn toggle_reviewed(&mut self, hunk: usize) -> Result<bool, EngineError> {
        let key = self
            .plan
            .hunk_key(plan::HunkId::from_index(hunk))
            .to_string();
        let now = self.state.reviewed_classes.insert(key.clone());
        if !now {
            self.state.reviewed_classes.remove(&key);
        }
        self.store.save_state(&self.state)?;
        Ok(now)
    }

    /// Mark a whole set of classes reviewed (or not) in one write.
    ///
    /// Set semantics, not toggle: a partially reviewed group resolves to the
    /// requested state instead of inverting member by member, and the batch
    /// costs one `save_state` rather than one per class.
    pub fn set_reviewed(&mut self, class_keys: &[String], on: bool) -> Result<(), EngineError> {
        for key in class_keys {
            if on {
                self.state.reviewed_classes.insert(key.clone());
            } else {
                self.state.reviewed_classes.remove(key);
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
        self.state.split_diff = on;
        self.store.save_state(&self.state)
    }

    /// Persist the left-pane view (semantic groups / flat file list).
    pub fn set_file_view(&mut self, on: bool) -> Result<(), EngineError> {
        self.state.file_view = on;
        self.store.save_state(&self.state)
    }

    /// Create a finding anchored on `hunk` and persist it.
    pub fn add_finding(&mut self, hunk: usize, body: String) -> Result<&Finding, EngineError> {
        let h = &self.doc.hunks[hunk];
        let side = if h.new_count > 0 { "new" } else { "old" };
        let line = if h.new_count > 0 {
            h.new_start.max(1)
        } else {
            h.old_start.max(1)
        };
        let vh = &self.view.hunks[hunk];
        let line_text = vh
            .added
            .first()
            .or(vh.removed.first())
            .map(|l| String::from_utf8_lossy(l).into_owned())
            .unwrap_or_default();
        let finding = Finding::new(
            crate::review_state::now_unix(),
            body,
            self.plan_hash.clone(),
            Anchor {
                file: h.file.clone(),
                side: side.into(),
                line,
                hunk_digest: h.digest.clone(),
                line_text,
            },
        );
        self.findings.push(finding);
        self.store.save_findings(&self.findings)?;
        Ok(self.findings.last().expect("just pushed"))
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
}
