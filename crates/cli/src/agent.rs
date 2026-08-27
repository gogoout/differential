//! `dfr agent` — what the grouping model asks, and how the answer reads.
//!
//! The engine answers the questions (`engine::artefact`); this renders them.
//! That is the same split every other consumer gets: `crates/stack` renders
//! commits, `crates/tui` renders a screen, and the model reads plain text.
//!
//! The reader here is a language model with a terminal, so the format is
//! compact and every line starts with the id it is about. Nothing is truncated
//! and nothing is capped: a question was asked, so the answer is the answer.

use std::path::Path;

use anyhow::Context;
use clap::Subcommand;

use differential_engine::artefact::{self, ClassView};
use differential_engine::gitio::Repo;
use differential_engine::model::DiffView;
use differential_engine::plan;
use differential_engine::schema::PlanDocument;

#[derive(Subcommand)]
pub enum Query {
    /// Every class: size, files, kind, what it defines and what it uses.
    Classes,
    /// One class in full, with every member hunk and every file.
    Class { id: String },
    /// The diff text of one hunk (`h12`) or of every hunk in a class (`C7`).
    Diff { id: String },
    /// The classes touching a path.
    File { path: String },
    /// The classes that introduce a symbol.
    Defines { symbol: String },
}

pub fn run(doc_path: &Path, repo_dir: &Path, query: &Query) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(doc_path)
        .with_context(|| format!("cannot read {}", doc_path.display()))?;
    let doc = PlanDocument::from_json(&text)
        .with_context(|| format!("{} is not a plan document", doc_path.display()))?;

    Ok(match query {
        Query::Classes => list(&doc, artefact::index(&doc), "no classes"),
        Query::Class { id } => match artefact::class(&doc, id) {
            Some(v) => detail(&doc, &v),
            None => format!("no class {id}\n"),
        },
        Query::File { path } => list(
            &doc,
            artefact::in_file(&doc, path),
            &format!("no class touches {path}"),
        ),
        Query::Defines { symbol } => list(
            &doc,
            artefact::definers(&doc, symbol),
            &format!("no class defines {symbol}"),
        ),
        Query::Diff { id } => {
            let Some(hunks) = artefact::resolve(&doc, id) else {
                return Ok(format!("no hunk or class {id}\n"));
            };
            diff(&doc, repo_dir, &hunks)?
        }
    })
}

// ------------------------------------------------------------------ classes

fn list(doc: &PlanDocument, views: Vec<ClassView<'_>>, empty: &str) -> String {
    if views.is_empty() {
        return format!("{empty}\n");
    }
    let mut out = String::new();
    for v in &views {
        out.push_str(&header(v));
        out.push('\n');
        for line in relations(doc, v) {
            out.push_str("     ");
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

fn detail(doc: &PlanDocument, v: &ClassView<'_>) -> String {
    let mut out = header(v);
    out.push('\n');
    for line in relations(doc, v) {
        out.push_str(&line);
        out.push('\n');
    }

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

// --------------------------------------------------------------------- diff

/// Diff text for a set of hunks.
///
/// The document records where a hunk is, never what it says, so the text comes
/// from re-enumerating the range the document names. Hunk ids are positional in
/// that enumeration, and the same range enumerates the same way every time —
/// which is what makes the lookup exact rather than approximate.
fn diff(
    doc: &PlanDocument,
    repo_dir: &Path,
    hunks: &[&differential_engine::schema::HunkEntry],
) -> anyhow::Result<String> {
    let repo = Repo::open(repo_dir)
        .with_context(|| format!("cannot open a repository at {}", repo_dir.display()))?;
    let view = enumerate(&repo, &doc.source.base, &doc.source.head)?;

    let mut out = String::new();
    for h in hunks {
        let index = plan::HunkId::parse(&h.id)
            .ok()
            .map(|p| p.index())
            .filter(|&i| i < view.hunks.len());
        let Some(hunk) = index.map(|i| &view.hunks[i]) else {
            out.push_str(&format!("{}: not in the range any more\n", h.id));
            continue;
        };
        out.push_str(&format!(
            "--- {}  {}  @@ -{},{} +{},{} @@\n",
            h.id, h.file, h.old_start, h.old_count, h.new_start, h.new_count
        ));
        for line in &hunk.removed {
            out.push_str(&format!("-{}\n", String::from_utf8_lossy(line)));
        }
        for line in &hunk.added {
            out.push_str(&format!("+{}\n", String::from_utf8_lossy(line)));
        }
    }
    Ok(out)
}

fn enumerate(repo: &Repo, base: &str, head: &str) -> anyhow::Result<DiffView> {
    use differential_engine::ports::DiffSource;
    let raw_records = repo.raw_records(base, head)?;
    let canonical_patch = repo.canonical_patch(base, head)?;
    let rename_records = repo.rename_records(base, head)?;
    Ok(plan::build_view(&plan::Enumeration {
        raw_records: &raw_records,
        canonical_patch: &canonical_patch,
        rename_records: &rename_records,
    })?)
}
