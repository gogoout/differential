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
pub fn review_id(base_sha: &str, head_spec: &str) -> String {
    let mut h = Sha1::new();
    h.update(base_sha.as_bytes());
    h.update([0]);
    h.update(head_spec.as_bytes());
    short_hash(h)
}

/// Reviewed marks key on what a class IS, not what it is called.
///
/// Sorted member digests, so the key survives regeneration reshuffling
/// positional ids — the same set of hunks is the same class however it is
/// numbered.
pub fn class_content_key(member_digests: &[String]) -> String {
    let mut sorted: Vec<&String> = member_digests.iter().collect();
    sorted.sort_unstable();
    let mut h = Sha1::new();
    for d in sorted {
        h.update(d.as_bytes());
        h.update([0]);
    }
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
    common_dir
        .join("differential")
        .join("cache")
        .join("grouping")
}

/// One review's sidecar directory, given the git common dir.
pub fn review_dir(common_dir: &Path, review_id: &str) -> PathBuf {
    common_dir
        .join("differential")
        .join("reviews")
        .join(review_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_review_survives_its_head_moving_but_not_a_different_branch() {
        // The reason identity uses the spec rather than the resolved sha.
        assert_eq!(review_id("abc", "feature"), review_id("abc", "feature"));
        assert_ne!(review_id("abc", "feature"), review_id("abc", "other"));
        assert_ne!(review_id("abc", "feature"), review_id("def", "feature"));
    }

    #[test]
    fn class_keys_ignore_member_order_but_not_membership() {
        let a = class_content_key(&["d2".into(), "d1".into()]);
        assert_eq!(a, class_content_key(&["d1".into(), "d2".into()]));
        assert_ne!(a, class_content_key(&["d1".into()]));
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
