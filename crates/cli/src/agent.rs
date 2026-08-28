//! `dfr agent` — what the grouping model reads, and how it reads.
//!
//! The engine answers the question (`engine::artefact`); this renders it. That
//! is the same split every other consumer gets: `crates/stack` renders commits,
//! `crates/tui` renders a screen, and the model reads plain text.
//!
//! **One command, one answer: every class the model may group, in full.** There
//! were five queries once — `classes`, `class`, `diff`, `file`, `defines`. Each
//! was a round trip through a model turn, and each answered a slice of one
//! document the engine already held whole.
//!
//! Two measurements collapsed them. `diff` re-enumerated the range to print
//! hunk text, which `git diff` does; it went, and with it the only reason this
//! command opened a repository (ADR 0022). What remained is 72KB for a
//! 196-class change — beside the 322KB of diff the model reads anyway, that is
//! not worth four commands and three extra turns to slice. So it is not sliced.
//! `file` and `defines` were lookups into a list the reader now has in front of
//! it.
//!
//! The reader is a language model with a terminal, so the format is compact and
//! every line starts with the id it is about. Nothing is truncated and nothing
//! is capped: the answer is the answer.

use std::path::Path;

use anyhow::Context;

use differential_engine::artefact::{self, ClassView};
use differential_engine::schema::PlanDocument;

/// Every class the model is asked to group, in full.
///
/// Generated content is left out, exactly as the prompt's id list leaves it out
/// (`plan::class_is_generated`, ADR 0006). Handing the model a lockfile would be
/// bytes it must read and a class id the audit would reject as a hallucination.
/// No class printed here touches a generated file at all: `generated` is part of
/// the shape-class key, so a class is wholly one or the other. The noise tier
/// still never hides — `git diff` reaches any path.
pub fn run(doc_path: &Path) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(doc_path)
        .with_context(|| format!("cannot read {}", doc_path.display()))?;
    let doc = PlanDocument::from_json(&text)
        .with_context(|| format!("{} is not a plan document", doc_path.display()))?;

    let offered = artefact::index(&doc);
    if offered.is_empty() {
        return Ok("no classes\n".to_string());
    }
    Ok(offered.iter().map(|v| detail(&doc, v)).collect())
}

fn detail(doc: &PlanDocument, v: &ClassView<'_>) -> String {
    let mut out = header(v);
    out.push('\n');
    for line in relations(doc, v) {
        out.push_str(&line);
        out.push('\n');
    }

    // Every member, not just the exemplar. Rating a class `skim` is a claim
    // about all of them, and this is the list the reader checks it against —
    // each line names a file and a line range, which is a `git diff` away from
    // the text itself.
    out.push_str("hunks:\n");
    for h in &v.members {
        let mark = if h.id == v.class.exemplar {
            "  (exemplar)"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {}  {}  @@ -{},{} +{},{} @@{mark}\n",
            h.id, h.file, h.old_start, h.old_count, h.new_start, h.new_count
        ));
    }

    out.push_str("files:\n");
    for path in &v.files {
        // The rename note is what the relocation rule turns on, so it travels
        // with the file rather than being something to go and look up.
        let note = doc
            .files
            .iter()
            .find(|f| &f.path == path)
            .and_then(|f| Some((f.old_path.as_deref()?, f.rename_similarity?)))
            .map(|(old, sim)| format!("  (renamed from {old}, {sim}% similar)"))
            .unwrap_or_default();
        out.push_str(&format!("  {path}{note}\n"));
    }
    out
}

fn header(v: &ClassView<'_>) -> String {
    format!(
        "{:<5} {}h {}f {:?}  {}  pure={}",
        v.class.id,
        v.members.len(),
        v.files.len(),
        v.kind,
        v.exemplar_at(),
        if v.class.pure_substitution {
            "yes"
        } else {
            "no"
        },
    )
}

/// What the class introduces, what it consumes, and who consumes it.
///
/// The reverse edges are printed too. They cost a scan of a list the caller
/// already has, and "who needs this" is the half of a dependency a reader
/// cannot work out from their own entry.
fn relations(doc: &PlanDocument, v: &ClassView<'_>) -> Vec<String> {
    let mut out = Vec::new();
    if !v.class.defines.is_empty() {
        out.push(format!("defines: {}", v.class.defines.join(", ")));
    }
    if !v.class.depends_on.is_empty() {
        let uses: Vec<String> = v
            .class
            .depends_on
            .iter()
            .map(|e| format!("{} ({})", e.on, e.via.join(" ")))
            .collect();
        out.push(format!("uses: {}", uses.join(", ")));
    }
    let used_by: Vec<&str> = doc
        .classes
        .iter()
        .filter(|c| c.depends_on.iter().any(|e| e.on == v.class.id))
        .map(|c| c.id.as_str())
        .collect();
    if !used_by.is_empty() {
        out.push(format!("used by: {}", used_by.join(", ")));
    }
    out
}
