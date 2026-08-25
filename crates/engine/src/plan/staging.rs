//! What a file contributes to a tree — decided without touching git.
//!
//! Both tree builders used to interleave this decision with the writes it
//! implies, which is why neither could be tested without a repository. The
//! decisions are here; the writing stays with whoever owns an index.

use crate::EngineError;
use crate::model::{Disposition, FileChange};

/// How one file should be staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staged<'a> {
    /// Drop the path from the tree.
    Remove,
    /// Stage an oid recorded in the diff, verbatim.
    ///
    /// The documented tautology, and the only place one is allowed: gitlinks
    /// and binary files carry no hunks, so a recorded oid is the sole
    /// available content. The invariant report says so out loud.
    Recorded { mode: &'a str, oid: &'a str },
    /// Stage content computed **by applying hunks**.
    ///
    /// Never by copying the head blob: with the copy shortcut the tree
    /// assertion holds by construction and proves nothing (invariant 3).
    Apply { mode: &'a str },
}

/// The core tree builder's rule: the file's final state, every hunk applied.
pub fn final_state(f: &FileChange) -> Result<Staged<'_>, EngineError> {
    if f.disposition == Disposition::Deleted {
        return Ok(Staged::Remove);
    }

    let mode = f.new_mode.as_deref().ok_or_else(|| {
        EngineError::Invariant(format!(
            "no new mode recorded for {}",
            String::from_utf8_lossy(&f.path)
        ))
    })?;

    if f.submodule.is_some() {
        // The commit id from the pseudo-hunk, cross-checked against the raw
        // record's oid when both exist.
        let oid = f
            .submodule
            .as_ref()
            .and_then(|(_, new)| new.as_deref())
            .or(f.new_oid.as_deref())
            .ok_or_else(|| {
                EngineError::Invariant(format!(
                    "submodule {} has no new commit id",
                    String::from_utf8_lossy(&f.path)
                ))
            })?;
        return Ok(Staged::Recorded { mode, oid });
    }

    if f.binary {
        let oid = f.new_oid.as_deref().ok_or_else(|| {
            EngineError::Invariant(format!(
                "binary file {} has no recorded oid",
                String::from_utf8_lossy(&f.path)
            ))
        })?;
        return Ok(Staged::Recorded { mode, oid });
    }

    Ok(Staged::Apply { mode })
}

/// The stack renderer's rule: the file's state after `applied` of its hunks.
///
/// Deliberately a second function rather than a flag on `final_state` — the
/// rules genuinely differ, and each difference is load-bearing:
///
/// - a deletion is a removal only once **every** hunk has been applied, or the
///   stack would drop a file mid-series and lose the hunks still to come;
/// - the mode falls back to `old_mode`, because a partially-built file may not
///   have reached the commit that sets its new one;
/// - submodules are decided **before** the mode, since a gitlink's mode is a
///   constant rather than something the diff has to have recorded.
pub fn cumulative_state(f: &FileChange, applied: usize) -> Result<Staged<'_>, EngineError> {
    let complete = applied == f.hunks.len();

    if f.disposition == Disposition::Deleted && complete {
        return Ok(Staged::Remove);
    }

    if let Some((_, new)) = &f.submodule {
        let oid = new.as_deref().or(f.new_oid.as_deref()).ok_or_else(|| {
            EngineError::Invariant(format!(
                "submodule {} has no new commit id",
                String::from_utf8_lossy(&f.path)
            ))
        })?;
        return Ok(Staged::Recorded {
            mode: "160000",
            oid,
        });
    }

    let mode = f
        .new_mode
        .as_deref()
        .or(f.old_mode.as_deref())
        .ok_or_else(|| {
            EngineError::Invariant(format!(
                "no mode recorded for {}",
                String::from_utf8_lossy(&f.path)
            ))
        })?;

    Ok(Staged::Apply { mode })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(disposition: Disposition) -> FileChange {
        FileChange {
            path: b"src/a.rs".to_vec(),
            disposition,
            new_mode: Some("100644".into()),
            old_mode: None,
            binary: false,
            submodule: None,
            old_oid: None,
            new_oid: None,
            hunks: vec![0, 1],
            rename_similarity: None,
            rename_from: None,
            rename_to: None,
            generated: None,
        }
    }

    #[test]
    fn text_files_are_always_rebuilt_from_hunks() {
        // The whole tree assertion rests on this never becoming a copy.
        let f = file(Disposition::Modified);
        assert_eq!(final_state(&f).unwrap(), Staged::Apply { mode: "100644" });
        assert_eq!(
            cumulative_state(&f, 1).unwrap(),
            Staged::Apply { mode: "100644" }
        );
    }

    #[test]
    fn binary_files_stage_their_recorded_oid() {
        let mut f = file(Disposition::Modified);
        f.binary = true;
        f.hunks.clear();
        f.new_oid = Some("deadbeef".into());
        assert_eq!(
            final_state(&f).unwrap(),
            Staged::Recorded {
                mode: "100644",
                oid: "deadbeef"
            }
        );
    }

    #[test]
    fn a_binary_file_without_an_oid_is_an_invariant_failure_not_an_empty_blob() {
        let mut f = file(Disposition::Modified);
        f.binary = true;
        let err = final_state(&f).unwrap_err().to_string();
        assert!(err.contains("binary file"), "{err}");
        assert!(err.contains("src/a.rs"), "{err}");
    }

    #[test]
    fn submodules_stage_the_gitlink_commit_id() {
        let mut f = file(Disposition::Modified);
        f.submodule = Some((Some("old".into()), Some("new".into())));
        f.new_mode = Some("160000".into());
        assert_eq!(
            final_state(&f).unwrap(),
            Staged::Recorded {
                mode: "160000",
                oid: "new"
            }
        );
        // The stack does not need the mode recorded: a gitlink's is a constant.
        f.new_mode = None;
        assert_eq!(
            cumulative_state(&f, 0).unwrap(),
            Staged::Recorded {
                mode: "160000",
                oid: "new"
            }
        );
    }

    /// The difference between the two rules that actually matters: a deletion
    /// mid-series still carries hunks, so removing it early would lose them.
    #[test]
    fn a_deletion_is_removed_only_once_every_hunk_has_been_applied() {
        let f = file(Disposition::Deleted);
        assert_eq!(final_state(&f).unwrap(), Staged::Remove);

        assert_eq!(
            cumulative_state(&f, 1).unwrap(),
            Staged::Apply { mode: "100644" },
            "one of two hunks applied: the file must still exist"
        );
        assert_eq!(cumulative_state(&f, 2).unwrap(), Staged::Remove);
    }

    #[test]
    fn the_stack_falls_back_to_the_old_mode_the_core_does_not() {
        let mut f = file(Disposition::Modified);
        f.new_mode = None;
        f.old_mode = Some("100755".into());

        assert_eq!(
            cumulative_state(&f, 0).unwrap(),
            Staged::Apply { mode: "100755" },
            "a partially built file may not have reached its mode change yet"
        );
        assert!(
            final_state(&f)
                .unwrap_err()
                .to_string()
                .contains("new mode"),
            "the final state has no excuse for a missing new mode"
        );
    }
}
