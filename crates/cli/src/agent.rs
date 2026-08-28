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

/// Any number of arguments per call, including none.
///
/// The reader is an agent, and every call is a round trip through a model turn.
/// Asked one id at a time it walked a two-hundred-class change one class per
/// turn, while the work behind each call stayed under a tenth of a second — so
/// the batch is the whole saving, and **no ids at all means the lot**. The
/// engine already holds every hunk; making a model reassemble that over a
/// one-turn-per-call channel would be round trips for nothing.
#[derive(Subcommand)]
pub enum Query {
    /// Every class: size, files, kind, what it defines and what it uses.
    Classes,
    /// Classes in full, with every member hunk and every file. No ids: all of
    /// them.
    Class {
        #[arg(num_args = 0..)]
        ids: Vec<String>,
    },
    /// The diff text of hunks (`h12`) or of every hunk in a class (`C7`). No
    /// ids: the whole change.
    Diff {
        #[arg(num_args = 0..)]
        ids: Vec<String>,
        /// Resume after this hunk id. A reply too large to send whole ends with
        /// the exact command that continues it.
        #[arg(long)]
        after: Option<String>,
    },
    /// The classes touching paths. No paths: every file.
    File {
        #[arg(num_args = 0..)]
        paths: Vec<String>,
    },
    /// The classes that introduce symbols. No symbols: every definition.
    Defines {
        #[arg(num_args = 0..)]
        symbols: Vec<String>,
    },
}

pub fn run(doc_path: &Path, repo_dir: &Path, query: &Query) -> anyhow::Result<String> {
    let text = std::fs::read_to_string(doc_path)
        .with_context(|| format!("cannot read {}", doc_path.display()))?;
    let doc = PlanDocument::from_json(&text)
        .with_context(|| format!("{} is not a plan document", doc_path.display()))?;

    Ok(match query {
        Query::Classes => list(&doc, artefact::index(&doc), "no classes"),
        Query::Class { ids } if ids.is_empty() => artefact::index(&doc)
            .iter()
            .map(|v| detail(&doc, v))
            .collect(),
        Query::Class { ids } => ids
            .iter()
            .map(|id| match artefact::class(&doc, id) {
                Some(v) => detail(&doc, &v),
                None => format!("no class {id}\n"),
            })
            .collect(),
        // No paths means every file the offered classes touch — not every file
        // in the change, which would sweep the generated ones back in.
        Query::File { paths } if paths.is_empty() => {
            let offered = artefact::index(&doc);
            let mut seen: Vec<&str> = offered.iter().flat_map(|v| v.files.clone()).collect();
            seen.sort_unstable();
            seen.dedup();
            seen.iter()
                .map(|path| {
                    list(
                        &doc,
                        artefact::in_file(&doc, path),
                        &format!("no class touches {path}"),
                    )
                })
                .collect()
        }
        Query::File { paths } => paths
            .iter()
            .map(|path| {
                list(
                    &doc,
                    artefact::in_file(&doc, path),
                    &format!("no class touches {path}"),
                )
            })
            .collect(),
        Query::Defines { symbols } if symbols.is_empty() => {
            let offered = artefact::index(&doc);
            let mut all: Vec<&str> = offered
                .iter()
                .flat_map(|v| v.class.defines.iter().map(String::as_str))
                .collect();
            all.sort_unstable();
            all.dedup();
            all.iter()
                .map(|sym| {
                    list(
                        &doc,
                        artefact::definers(&doc, sym),
                        &format!("no class defines {sym}"),
                    )
                })
                .collect()
        }
        Query::Defines { symbols } => symbols
            .iter()
            .map(|symbol| {
                list(
                    &doc,
                    artefact::definers(&doc, symbol),
                    &format!("no class defines {symbol}"),
                )
            })
            .collect(),
        Query::Diff { ids, after } => diff(&doc, repo_dir, ids, after.as_deref(), doc_path)?,
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

/// Diff text for every hunk the given ids name.
///
/// The document records where a hunk is, never what it says, so the text comes
/// from re-enumerating the range the document names. Hunk ids are positional in
/// that enumeration, and the same range enumerates the same way every time —
/// which is what makes the lookup exact rather than approximate.
///
/// The enumeration happens once for the whole batch. It is three git calls and
/// a parse, so a batch of twenty costs what one used to.
fn diff(
    doc: &PlanDocument,
    repo_dir: &Path,
    ids: &[String],
    after: Option<&str>,
    doc_path: &Path,
) -> anyhow::Result<String> {
    let mut hunks: Vec<&differential_engine::schema::HunkEntry> = Vec::new();
    let mut unknown = String::new();
    if ids.is_empty() {
        // No ids means the whole change, minus generated content — the same
        // set the grouping stage offers, because a reply that hands the model
        // a lockfile it may not name is bytes it must read and cannot use
        // (`plan::hunk_is_generated`, ADR 0006). Naming one still serves it.
        hunks.extend(
            doc.hunks
                .iter()
                .filter(|h| !differential_engine::plan::hunk_is_generated(doc, h)),
        );
    } else {
        for id in ids {
            match artefact::resolve(doc, id) {
                Some(found) => hunks.extend(found),
                None => unknown.push_str(&format!("no hunk or class {id}\n")),
            }
        }
    }

    // A cursor resumes the same list, so it needs no encoding: a hunk id
    // already names a position in it.
    if let Some(mark) = after {
        match hunks.iter().position(|h| h.id == mark) {
            Some(i) => {
                hunks.drain(..=i);
            }
            None => return Ok(format!("{unknown}{mark} is not in this list\n")),
        }
    }
    if hunks.is_empty() {
        // An empty reply to a legitimate "continue" reads as a failure, so a
        // finished cursor says it is finished.
        if after.is_some() {
            unknown.push_str("no more hunks: that was the end of the list\n");
        }
        return Ok(unknown);
    }

    render_diff(doc, repo_dir, &hunks, unknown, ids, doc_path)
}

/// One reply carries at most this much diff text.
///
/// The cap bounds a reply, never the change: what does not fit comes back with
/// the command that continues it, so nothing is dropped for length. That was
/// the failure of the old 90,000-character prompt cap, which silently pushed
/// whole classes into the back-fill.
///
/// Roughly 64k tokens — about a third of a large context window, so a reply is
/// substantial without being all the reader can hold.
const MAX_REPLY_BYTES: usize = 256 * 1024;

fn render_diff(
    doc: &PlanDocument,
    repo_dir: &Path,
    hunks: &[&differential_engine::schema::HunkEntry],
    unknown: String,
    ids: &[String],
    doc_path: &Path,
) -> anyhow::Result<String> {
    let repo = Repo::open(repo_dir)
        .with_context(|| format!("cannot open a repository at {}", repo_dir.display()))?;
    let view = enumerate(&repo, &doc.source.base, &doc.source.head)?;

    let mut out = unknown;
    for (i, h) in hunks.iter().enumerate() {
        // Checked before writing, not after, so a reply never exceeds the cap
        // by most of a hunk. The first hunk always goes out, however big it is:
        // a cursor that cannot advance is worse than an oversized reply.
        if i > 0 && out.len() >= MAX_REPLY_BYTES {
            out.push_str(&resume_line(hunks[i - 1].id.as_str(), ids, doc_path));
            return Ok(out);
        }
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

/// The exact command that continues a reply the cap cut short.
///
/// Spelled out in full rather than described. The reader is an agent with a
/// terminal, and a command it can run beats an instruction it has to assemble.
fn resume_line(last: &str, ids: &[String], doc_path: &Path) -> String {
    let exe = std::env::args().next().unwrap_or_else(|| "dfr".to_string());
    let named = if ids.is_empty() {
        String::new()
    } else {
        format!(" {}", ids.join(" "))
    };
    format!(
        "\n[reply full. continue with]\n  {exe} agent --doc {} diff{named} --after {last}\n",
        doc_path.display()
    )
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
