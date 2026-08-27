//! What the model is given, and how it asks for more (ADR 0022).
//!
//! The grouping stage used to hand the model a fixed string: one block per
//! shape class, eight lines of diff from the exemplar, six basenames. So a
//! class of nine hunks was rated `skim` — "read one, trust the rest" — on the
//! evidence of one hunk, and the prompt had a character cap that silently
//! truncated large changes.
//!
//! Now the engine writes the pre-group document to a file and the model
//! **fetches** what it needs. Its job is unchanged: it merges class ids, labels
//! and rates, never touching hunks (ADR 0001). What changed is the context it
//! has to do that job with.
//!
//! This module owns all of it — building the class graph ([`graph`]), and
//! answering the queries behind `dfr agent`. It returns data; rendering it as
//! text is `crates/cli`'s job, the same as for every other consumer.

pub mod graph;

use crate::plan::HunkId;
use crate::schema;

/// One class, resolved: everything a caller needs to describe or drill into it.
///
/// One type for both the index and the detail view, because the index is the
/// same facts with fewer of them printed.
pub struct ClassView<'d> {
    pub class: &'d schema::ClassEntry,
    /// Member hunks, in class order.
    pub members: Vec<&'d schema::HunkEntry>,
    /// The member a reviewer reads to verify the whole class.
    pub exemplar: &'d schema::HunkEntry,
    /// Distinct paths the class touches, in first-seen order.
    pub files: Vec<&'d str>,
    /// Disposition of the exemplar's file.
    pub kind: schema::Disposition,
}

impl ClassView<'_> {
    /// `path:line` for the exemplar — where to go and look.
    pub fn exemplar_at(&self) -> String {
        format!("{}:{}", self.exemplar.file, self.exemplar.new_start.max(1))
    }
}

/// Every class, largest first — the order the class ids already carry.
pub fn index(doc: &schema::PlanDocument) -> Vec<ClassView<'_>> {
    doc.classes.iter().filter_map(|c| view(doc, c)).collect()
}

/// One class by id.
pub fn class<'d>(doc: &'d schema::PlanDocument, id: &str) -> Option<ClassView<'d>> {
    view(doc, doc.classes.iter().find(|c| c.id == id)?)
}

/// The classes touching `path`, largest first.
pub fn in_file<'d>(doc: &'d schema::PlanDocument, path: &str) -> Vec<ClassView<'d>> {
    index(doc)
        .into_iter()
        .filter(|v| v.files.contains(&path))
        .collect()
}

/// The classes that define `symbol`.
///
/// Answers from the graph the mechanism already computed. It does not resolve
/// symbols on demand: a caller asking twice must get the same answer as the
/// ordering stage acted on.
pub fn definers<'d>(doc: &'d schema::PlanDocument, symbol: &str) -> Vec<ClassView<'d>> {
    index(doc)
        .into_iter()
        .filter(|v| v.class.defines.iter().any(|d| d == symbol))
        .collect()
}

/// The hunks a `hN` or `Cn` id names, in canonical order.
///
/// The one query that reaches past the document: a hunk entry records where a
/// hunk is, never what it says. The caller re-enumerates the recorded range to
/// get the text, which is why hunk ids being positional is safe here — the
/// same range enumerates to the same order every time.
pub fn resolve<'d>(doc: &'d schema::PlanDocument, id: &str) -> Option<Vec<&'d schema::HunkEntry>> {
    if let Ok(h) = HunkId::parse(id) {
        return doc.hunks.get(h.index()).map(|entry| vec![entry]);
    }
    let c = doc.classes.iter().find(|c| c.id == id)?;
    Some(hunks_of(doc, &c.hunk_ids))
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
    })
}

fn hunks_of<'d>(doc: &'d schema::PlanDocument, ids: &[String]) -> Vec<&'d schema::HunkEntry> {
    ids.iter()
        .filter_map(|hid| HunkId::parse(hid).ok())
        .filter_map(|h: HunkId| doc.hunks.get(h.index()))
        .collect()
}
