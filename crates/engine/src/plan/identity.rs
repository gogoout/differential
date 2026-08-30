//! Review identity and state locations (ADR 0013, `spec/persistence.md`).
//!
//! All pure: what a review is *called* and *where its state lives* are domain
//! decisions. Only actually reading and writing there is the adapter's.

use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

/// Truncated sha1, hex. Sixteen characters is plenty to key a per-repo
/// directory and short enough to read in a path.
fn short_hash(h: Sha1) -> String {
    hex::encode(h.finalize())[..16].to_string()
}

/// A review's identity: the resolved base sha plus the head spec AS TYPED.
///
/// The spec, not the resolved head: a branch name keeps one review alive as
/// its tip moves, where a sha would file every commit as a new review and
/// strand the reviewer's progress behind it.
///
/// The spelling is therefore part of the name, which is why one range typed
/// two ways used to be two reviews. `review_identity::resolve` closes that:
/// it adopts an existing review on the same line of history and records the
/// join, so this stays a pure function of the two strings.
pub fn review_id(base_sha: &str, head_spec: &str) -> String {
    let mut h = Sha1::new();
    h.update(base_sha.as_bytes());
    h.update([0]);
    h.update(head_spec.as_bytes());
    short_hash(h)
}

/// A review the reader named. The name IS the identity, so neither endpoint
/// is in the key: rebase the base or the head and the session survives, the
/// way a pull request survives a force-push because it is an object rather
/// than a range.
///
/// The leading tag byte keeps a name out of `review_id`'s space. That hash
/// starts with a resolved sha — hex ASCII — so it can never begin with `0x01`.
pub fn review_id_named(name: &str) -> String {
    let mut h = Sha1::new();
    h.update([1]);
    h.update(name.as_bytes());
    short_hash(h)
}

/// A plan document's content hash — its immutable identity, and what findings
/// record so re-anchoring knows which document they were written against.
pub fn plan_hash(json: &str) -> String {
    let mut h = Sha1::new();
    h.update(json.as_bytes());
    short_hash(h)
}

/// The grouping cache directory for a repo, given its git common dir.
///
/// Under the common dir, not the worktree's `.git`, so worktrees of one repo
/// share a cache — and outside the tracked tree, since a grouping is a local
/// artefact and not something to commit (ADR 0009).
pub fn grouping_cache_dir(common_dir: &Path) -> PathBuf {
    cache_dir(common_dir).join("grouping")
}

/// Everything regenerable, in ONE subtree.
///
/// `reviews/` is deliberately a sibling rather than a child. A review's
/// findings are the reader's own work and cannot be recomputed, so nothing that
/// clears the cache may be able to reach them — and with the two rooted apart,
/// that is a property of the layout rather than of a command remembering to be
/// careful (ADR 0009, ADR 0013).
pub fn cache_dir(common_dir: &Path) -> PathBuf {
    common_dir.join("differential").join("cache")
}

/// Where the pre-group document is left for the model to read, given the git
/// common dir.
///
/// A sibling of the grouping cache and for the same reasons: shared across
/// worktrees, and outside the tracked tree because it describes one local run
/// (ADR 0009, ADR 0022).
pub fn artefact_dir(common_dir: &Path) -> PathBuf {
    cache_dir(common_dir).join("document")
}

/// Where every review is filed. The directory is the catalogue: there is no
/// list file beside it that could disagree with what is on disk.
pub fn reviews_dir(common_dir: &Path) -> PathBuf {
    common_dir.join("differential").join("reviews")
}

/// One review's sidecar directory, given the git common dir.
pub fn review_dir(common_dir: &Path, review_id: &str) -> PathBuf {
    reviews_dir(common_dir).join(review_id)
}

/// What a review was opened as: the base sha and the head spec as typed.
///
/// Adoption needs the spelling back to ask git what it means now, and the id
/// is a hash, so the id cannot answer that. Written on open.
pub fn identity_path(common_dir: &Path, review_id: &str) -> PathBuf {
    review_dir(common_dir, review_id).join("identity.json")
}

/// A redirect: this id's progress lives under the id named inside.
///
/// Present only in a directory that adopted another review, and read before
/// anything else, so the join costs one file read and no git call.
pub fn alias_path(common_dir: &Path, review_id: &str) -> PathBuf {
    review_dir(common_dir, review_id).join("alias")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_review_survives_its_head_moving_but_not_a_different_branch() {
        // The reason identity uses the spec rather than the resolved sha.
        // Two spellings of one commit are joined by adoption, not here.
        assert_eq!(review_id("abc", "feature"), review_id("abc", "feature"));
        assert_ne!(review_id("abc", "feature"), review_id("abc", "other"));
        assert_ne!(review_id("abc", "feature"), review_id("def", "feature"));
    }

    #[test]
    fn state_lives_under_the_common_dir_not_the_tracked_tree() {
        let common = Path::new("/repo/.git");
        assert!(
            grouping_cache_dir(common).ends_with("differential/cache/grouping"),
            "{:?}",
            grouping_cache_dir(common)
        );
        assert!(
            review_dir(common, "abc123").ends_with("differential/reviews/abc123"),
            "{:?}",
            review_dir(common, "abc123")
        );
    }
}
