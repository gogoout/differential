//! Tree snapshots of uncommitted state (ADR 0017): the index and the worktree
//! as plain tree oids, so the rest of the pipeline — diff-tree enumeration,
//! the invariants, blob reads — runs on them unchanged.
//!
//! Plumbing only, and never the user's index: a temporary `GIT_INDEX_FILE` is
//! seeded from `ls-files -s -z` piped into `update-index -z --index-info`.
//! Snapshot blobs land in the odb (unreferenced, gc-able), which pins the
//! content so later `cat-file` reads by `<tree>:<path>` always resolve.

use crate::EngineError;
use crate::ports::{IndexSession, TreeBuilder, WorkingCopy};

/// Tree oid of the current index (the staged state). The user's index file is
/// never written to.
pub fn index_tree<G: TreeBuilder>(git: &G) -> Result<String, EngineError> {
    git.begin_from_current_index()?.write_tree()
}

/// Tree oid of the worktree: every tracked file's current content plus
/// untracked-but-not-ignored files, with worktree deletions honoured.
pub fn worktree_tree<G>(git: &G) -> Result<String, EngineError>
where
    G: TreeBuilder + WorkingCopy,
{
    let mut session = git.begin_from_current_index()?;
    // Union of tracked + untracked-unignored paths, NUL-delimited: `--add`
    // admits new files, `--remove` drops ones deleted from the worktree.
    let mut paths = git.tracked_paths()?;
    paths.extend_from_slice(&git.untracked_paths()?);
    session.stage_from_worktree(&paths)?;
    session.write_tree()
}
