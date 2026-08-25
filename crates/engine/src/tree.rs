//! Build the final tree from applied hunks, via plumbing against a temporary
//! index (ADR 0011). No checkout, no touching the user's index or worktree.
//!
//! Text file content is always computed BY APPLYING HUNKS — never by copying
//! head blobs — so tree equality against `head^{tree}` is a real assertion
//! (invariant 3). The two exceptions, both explicit here: binary files are
//! staged from the recorded head oid (they carry no hunks; documented
//! tautology), and submodules are staged as gitlinks from the pseudo-hunk's
//! commit id.

use std::ffi::OsStr;

use crate::EngineError;
use crate::apply::apply_hunks;
use crate::gitio::Repo;
use crate::model::{DiffView, Hunk};
use crate::plan;

pub(crate) const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// One `update-index -z --index-info` record: `<mode> <oid>\t<path>`.
pub fn index_entry(mode: &str, oid: &str, path: &[u8]) -> Vec<u8> {
    let mut line = format!("{mode} {oid}\t").into_bytes();
    line.extend_from_slice(path);
    line
}

/// Removal record (mode 0).
pub fn removal_entry(path: &[u8]) -> Vec<u8> {
    index_entry("0", ZERO_OID, path)
}

/// Stage every file's final state on top of `base` and return the written tree.
pub fn build_tree(repo: &Repo, base: &str, view: &DiffView) -> Result<String, EngineError> {
    let idx = tempfile::NamedTempFile::new().map_err(|e| EngineError::GitSpawn { source: e })?;
    let env: [(&str, &OsStr); 1] = [("GIT_INDEX_FILE", idx.path().as_os_str())];

    repo.run_env(["read-tree", base], None, &env)?;

    // One bulk `update-index -z --index-info` feed: quoting-proof and fast.
    let mut feed: Vec<u8> = Vec::new();
    for f in &view.files {
        let entry = staging_entry(repo, base, view, f)?;
        feed.extend_from_slice(&entry);
        feed.push(0);
    }
    repo.run_env(["update-index", "-z", "--index-info"], Some(&feed), &env)?;

    let out = repo.run_env(["write-tree"], None, &env)?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Compute one file's index record: decide with `plan::final_state`, then
/// perform whatever that decision implies.
fn staging_entry(
    repo: &Repo,
    base: &str,
    view: &DiffView,
    f: &crate::model::FileChange,
) -> Result<Vec<u8>, EngineError> {
    let path = f.path.as_slice();
    match plan::final_state(f)? {
        plan::Staged::Remove => Ok(removal_entry(path)),
        plan::Staged::Recorded { mode, oid } => Ok(index_entry(mode, oid, path)),
        plan::Staged::Apply { mode } => {
            let hunks: Vec<&Hunk> = f.hunks.iter().map(|&i| &view.hunks[i]).collect();
            let base_content = repo.blob(base, path)?;
            let content = apply_hunks(base_content.as_deref(), &hunks);
            let out = repo.run(["hash-object", "-w", "--stdin"], Some(&content))?;
            let oid = String::from_utf8_lossy(&out).trim().to_string();
            Ok(index_entry(mode, &oid, path))
        }
    }
}
