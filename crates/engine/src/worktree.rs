//! Tree snapshots of uncommitted state (ADR 0017): the index and the worktree
//! as plain tree oids, so the rest of the pipeline — diff-tree enumeration,
//! the invariants, blob reads — runs on them unchanged.
//!
//! Plumbing only, and never the user's index: a temporary `GIT_INDEX_FILE` is
//! seeded from `ls-files -s -z` piped into `update-index -z --index-info`.
//! Snapshot blobs land in the odb (unreferenced, gc-able), which pins the
//! content so later `cat-file` reads by `<tree>:<path>` always resolve.

use std::ffi::OsStr;

use crate::EngineError;
use crate::gitio::Repo;

/// Tree oid of the current index (the staged state). The user's index file is
/// never written to.
pub fn index_tree(repo: &Repo) -> Result<String, EngineError> {
    let (idx, env_val) = temp_index()?;
    let env: [(&str, &OsStr); 1] = [("GIT_INDEX_FILE", &env_val)];
    seed_from_index(repo, &env)?;
    let _keep = idx;
    write_tree(repo, &env)
}

/// Tree oid of the worktree: every tracked file's current content plus
/// untracked-but-not-ignored files, with worktree deletions honoured.
pub fn worktree_tree(repo: &Repo) -> Result<String, EngineError> {
    let (idx, env_val) = temp_index()?;
    let env: [(&str, &OsStr); 1] = [("GIT_INDEX_FILE", &env_val)];
    seed_from_index(repo, &env)?;

    // Union of tracked + untracked-unignored paths, NUL-delimited. `--add`
    // admits new files, `--remove` drops ones deleted from the worktree;
    // update-index hashes current content and writes the blobs.
    let mut paths = repo.run(["ls-files", "-z"], None)?;
    paths.extend_from_slice(&repo.run(["ls-files", "--others", "--exclude-standard", "-z"], None)?);
    if !paths.is_empty() {
        repo.run_env(
            ["update-index", "--add", "--remove", "-z", "--stdin"],
            Some(&paths),
            &env,
        )?;
    }
    let _keep = idx;
    write_tree(repo, &env)
}

/// A path for the temp index that does NOT exist yet — git treats an
/// existing empty file as a corrupt index, so hand it a fresh name inside a
/// temp dir rather than a pre-created NamedTempFile.
fn temp_index() -> Result<(tempfile::TempDir, std::ffi::OsString), EngineError> {
    let dir = tempfile::TempDir::new().map_err(|e| EngineError::GitSpawn { source: e })?;
    let path = dir.path().join("index").into_os_string();
    Ok((dir, path))
}

/// Copy the real index's entries into the temp index. Errors on unmerged
/// entries — a conflicted index has no single tree.
fn seed_from_index(repo: &Repo, env: &[(&str, &OsStr)]) -> Result<(), EngineError> {
    // "<mode> <oid> <stage>\t<path>" records — exactly the second input
    // format `update-index --index-info` accepts, so the seed is a byte pipe.
    let entries = repo.run(["ls-files", "-s", "-z"], None)?;
    for record in entries.split(|&b| b == 0) {
        let meta = record.split(|&b| b == b'\t').next().unwrap_or(record);
        if meta.ends_with(b" 1") || meta.ends_with(b" 2") || meta.ends_with(b" 3") {
            return Err(EngineError::Range(
                "index has unmerged entries — resolve conflicts before reviewing \
                 uncommitted changes"
                    .into(),
            ));
        }
    }
    if !entries.is_empty() {
        repo.run_env(["update-index", "-z", "--index-info"], Some(&entries), env)?;
    }
    Ok(())
}

fn write_tree(repo: &Repo, env: &[(&str, &OsStr)]) -> Result<String, EngineError> {
    let out = repo.run_env(["write-tree"], None, env)?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}
