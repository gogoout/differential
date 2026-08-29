//! Which review a range opens: the id, and whose progress it inherits.
//!
//! A review's id is `sha1(base_sha ‖ NUL ‖ head_spec)` with the head spec AS
//! TYPED (`plan::review_id`), which keeps one review alive while a branch tip
//! moves. The cost was that one range typed two ways was two reviews: mark
//! `<base>..<sha>`, reopen `<base>..HEAD` where HEAD *is* that sha, and the
//! progress was filed under a name you did not type again.
//!
//! So a new spelling adopts an existing review when the two are the same work:
//! the same base, and one head reachable from the other. That covers both the
//! two spellings of one commit and the commit you added since. Two branches
//! off one base are not adoptable — neither head reaches the other — so their
//! findings never collide.
//!
//! Adoption is silent and permanent. It is recorded as a redirect, so every
//! later open costs one file read, and `dfr findings` reaches the same review
//! the reviewer is looking at.

use crate::EngineError;
use crate::plan;
use crate::ports::{Ancestry, FiledReview, ReviewCatalogue, ReviewIdentity};

/// The head spec of a review of uncommitted work (ADR 0017). Its head is a
/// synthesized tree that churns on every edit, not a commit, so it can neither
/// adopt nor be adopted.
pub const WORKTREE_SPEC: &str = "WORKTREE";

/// The review directory `base_sha`/`head_spec` should read and write.
///
/// Returns the id of an existing review when this spelling names the same work,
/// and records the join. Otherwise returns this spelling's own id, and records
/// what it was opened as so a later spelling can find it.
pub fn resolve<C: ReviewCatalogue, G: Ancestry>(
    catalogue: &C,
    git: &G,
    base_sha: &str,
    head_spec: &str,
) -> Result<String, EngineError> {
    let id = plan::review_id(base_sha, head_spec);

    // A join already recorded. One file read, no scan and no git call.
    if let Some(target) = catalogue.alias_of(&id)? {
        return Ok(target);
    }

    let opened_as = ReviewIdentity {
        base: base_sha.to_string(),
        head_spec: head_spec.to_string(),
    };
    let filed = catalogue.filed_reviews()?;
    let known = filed.iter().find(|r| r.id == id);

    // An uncommitted head is a tree, not a commit, so ancestry says nothing
    // about it. Its identity is never written, which keeps it out of every
    // later scan by construction.
    if head_spec == WORKTREE_SPEC {
        return Ok(id);
    }

    if let Some(known) = known {
        // Already this reviewer's review. Record what it was opened as when
        // the directory does not say yet, so a review filed before identities
        // existed becomes adoptable rather than staying invisible forever —
        // and so the steady state costs no write.
        if known.opened_as.is_none() {
            catalogue.file_identity(&id, &opened_as)?;
        }
        return Ok(id);
    }

    match adopt(&filed, &opened_as, git)? {
        Some(target) => {
            catalogue.file_alias(&id, &target)?;
            Ok(target)
        }
        None => {
            catalogue.file_identity(&id, &opened_as)?;
            Ok(id)
        }
    }
}

/// The review `want` should inherit, if any.
///
/// Cheapest test first: the base is a string compare and drops every review of
/// other work before a single git process starts.
fn adopt<G: Ancestry>(
    filed: &[FiledReview],
    want: &ReviewIdentity,
    git: &G,
) -> Result<Option<String>, EngineError> {
    let Some(head) = git.commit_of(&want.head_spec)? else {
        return Ok(None);
    };

    let mut exact: Option<&str> = None;
    // The newest candidate this head has moved past, and the oldest that has
    // moved past it. Both are on one line of history with `head`, so "newest"
    // and "oldest" are ancestry, not clock time.
    let mut behind: Option<(&str, String)> = None;
    let mut ahead: Option<(&str, String)> = None;

    for r in filed {
        let Some(other) = r.opened_as.as_ref() else {
            continue;
        };
        if other.base != want.base || other.head_spec == WORKTREE_SPEC {
            continue;
        }
        // A branch deleted since the review was filed cannot be placed.
        let Some(other_head) = git.commit_of(&other.head_spec)? else {
            continue;
        };
        if other_head == head {
            exact = Some(&r.id);
            break;
        }
        if git.is_ancestor(&other_head, &head)? {
            let newer = match &behind {
                Some((_, best)) => git.is_ancestor(best, &other_head)?,
                None => true,
            };
            if newer {
                behind = Some((&r.id, other_head));
            }
        } else if git.is_ancestor(&head, &other_head)? {
            let older = match &ahead {
                Some((_, best)) => git.is_ancestor(&other_head, best)?,
                None => true,
            };
            if older {
                ahead = Some((&r.id, other_head));
            }
        }
    }

    Ok(exact
        .or(behind.map(|(id, _)| id))
        .or(ahead.map(|(id, _)| id))
        .map(str::to_string))
}
