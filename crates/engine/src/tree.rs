//! Build the final tree from applied hunks, via plumbing against a temporary
//! index (ADR 0011). No checkout, no touching the user's index or worktree.
//!
//! Text file content is always computed BY APPLYING HUNKS — never by copying
//! head blobs — so tree equality against `head^{tree}` is a real assertion
//! (invariant 3). The two exceptions, both explicit here: binary files are
//! staged from the recorded head oid (they carry no hunks; documented
//! tautology), and submodules are staged as gitlinks from the pseudo-hunk's
//! commit id.

use crate::EngineError;
use crate::apply::apply_hunks;
use crate::model::{DiffView, Hunk};
use crate::plan;
use crate::ports::{IndexEntry, IndexSession, ObjectReader, ObjectWriter, TreeBuilder};

/// Stage every file's final state on top of `base` and return the written tree.
pub fn build_tree<G>(git: &G, base: &str, view: &DiffView) -> Result<String, EngineError>
where
    G: ObjectReader + ObjectWriter + TreeBuilder,
{
    let mut session = git.begin_from_tree(base)?;
    // One batch: quoting-proof, and one subprocess instead of one per file.
    let mut entries = Vec::with_capacity(view.files.len());
    for f in &view.files {
        entries.push(staging_entry(git, base, view, f)?);
    }
    session.stage(&entries)?;
    session.write_tree()
}

/// Compute one file's index record: decide with `plan::final_state`, then
/// perform whatever that decision implies.
fn staging_entry<G>(
    git: &G,
    base: &str,
    view: &DiffView,
    f: &crate::model::FileChange,
) -> Result<IndexEntry, EngineError>
where
    G: ObjectReader + ObjectWriter,
{
    let path = f.path.clone();
    match plan::final_state(f)? {
        plan::Staged::Remove => Ok(IndexEntry::Remove { path }),
        plan::Staged::Recorded { mode, oid } => Ok(IndexEntry::Set {
            mode: mode.to_string(),
            oid: oid.to_string(),
            path,
        }),
        plan::Staged::Apply { mode } => {
            let hunks: Vec<&Hunk> = f.hunks.iter().map(|&i| &view.hunks[i]).collect();
            let base_content = git.blob(base, &path)?;
            let content = apply_hunks(base_content.as_deref(), &hunks);
            Ok(IndexEntry::Set {
                mode: mode.to_string(),
                oid: git.write_blob(&content)?,
                path,
            })
        }
    }
}
