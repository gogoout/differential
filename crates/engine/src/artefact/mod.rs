//! What the model is given (ADR 0022).
//!
//! The grouping stage used to hand the model a fixed string: one block per
//! shape class, eight lines of diff from the exemplar, six basenames. So a
//! class of nine hunks was rated `skim` — "read one, trust the rest" — on the
//! evidence of one hunk, and the prompt had a character cap that silently
//! truncated large changes.
//!
//! Now the engine writes the pre-group document to a file and the model
//! **fetches** the whole class table from it in one call. Its job is unchanged:
//! it merges class ids, labels and rates, never touching hunks (ADR 0001). What
//! changed is the context it has to do that job with.
//!
//! Every answer here comes from the document. A hunk entry records where a hunk
//! is, never what it says, and the text is `git diff`'s job — so nothing in
//! this module reaches a repository.
//!
//! This module owns all of it — building the class graph ([`graph`]), and
//! answering the one question behind `dfr agent`. It returns data; rendering it
//! as text is `crates/cli`'s job, the same as for every other consumer.

pub mod graph;

use crate::plan::HunkId;
use crate::schema;

/// One class, resolved: everything a caller needs to describe it.
pub struct ClassView<'d> {
    pub class: &'d schema::ClassEntry,
    /// Member hunks, in class order.
    pub members: Vec<&'d schema::HunkEntry>,
    /// The member a reviewer reads to verify the whole class.
    pub exemplar: &'d schema::HunkEntry,
    /// Distinct paths the class touches, in first-seen order.
    pub files: Vec<&'d str>,
    /// The subset of `files` the classification pass marked generated.
    ///
    /// A class whose files are ALL generated never reaches [`index`] at all.
    /// This is the mixed class — some generated files, some not — which stays
    /// with the model by design (ADR 0006). The model reads diff text with
    /// `git diff` now, and `git diff` honours no tier, so which paths the stage
    /// folds away is something it has to be told rather than something it can
    /// infer from what it was shown.
    pub generated: Vec<&'d str>,
    /// Disposition of the exemplar's file.
    pub kind: schema::Disposition,
}

impl ClassView<'_> {
    /// `path:line` for the exemplar — where to go and look.
    pub fn exemplar_at(&self) -> String {
        format!("{}:{}", self.exemplar.file, self.exemplar.new_start.max(1))
    }
}

/// Every class the model is asked to group, largest first — the order the class
/// ids already carry.
///
/// **This is the whole read path.** There were four more — one class by id, the
/// classes touching a path, the classes defining a symbol, and every class
/// generated included. Each was a lookup into this list, at a model turn per
/// call, and the list is 72KB for a 196-class change. So the list goes out
/// whole and the lookups go.
///
/// **Generated content is left out**, exactly as the grouping stage leaves it
/// out of the prompt (`plan::class_is_generated`, ADR 0006). Listing a class the
/// model may not name would invite it to name one, and the audit would throw
/// that whole group away as a hallucination. The noise tier still folds rather
/// than hides: `git diff` reaches any path at all, and a class that has a
/// generated file among its own says so in [`ClassView::generated`].
pub fn index(doc: &schema::PlanDocument) -> Vec<ClassView<'_>> {
    all(doc)
        .into_iter()
        .filter(|v| !crate::plan::class_is_generated(doc, v.class))
        .collect()
}

fn all(doc: &schema::PlanDocument) -> Vec<ClassView<'_>> {
    doc.classes.iter().filter_map(|c| view(doc, c)).collect()
}

fn view<'d>(doc: &'d schema::PlanDocument, class: &'d schema::ClassEntry) -> Option<ClassView<'d>> {
    let members = hunks_of(doc, &class.hunk_ids);
    let exemplar = doc
        .hunks
        .get(HunkId::parse(&class.exemplar).ok()?.index())?;
    let mut files: Vec<&str> = Vec::new();
    for m in &members {
        if !files.contains(&m.file.as_str()) {
            files.push(&m.file);
        }
    }
    let generated: Vec<&str> = files
        .iter()
        .copied()
        .filter(|path| crate::plan::file_is_generated(doc, path))
        .collect();
    Some(ClassView {
        kind: doc
            .files
            .iter()
            .find(|f| f.path == exemplar.file)
            .map(|f| f.disposition)?,
        class,
        members,
        exemplar,
        files,
        generated,
    })
}

fn hunks_of<'d>(doc: &'d schema::PlanDocument, ids: &[String]) -> Vec<&'d schema::HunkEntry> {
    ids.iter()
        .filter_map(|hid| HunkId::parse(hid).ok())
        .filter_map(|h: HunkId| doc.hunks.get(h.index()))
        .collect()
}
