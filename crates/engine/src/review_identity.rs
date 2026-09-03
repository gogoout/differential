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
//!
//! Adoption cannot cover a rebase: rewriting commits changes both endpoints,
//! so neither the base nor the head is reachable from its old self. A reader
//! who wants a session that outlives a rebase **names** it, and the name is
//! then the whole identity — the way a pull request survives a force-push
//! because it is an object rather than a range (ADR 0027).

use crate::EngineError;
use crate::plan;
use crate::ports::{Ancestry, FiledReview, ReviewCatalogue, ReviewIdentity};

/// The head spec of a review of uncommitted work (ADR 0017). Its head is a
/// synthesized tree that churns on every edit, not a commit, so it can neither
/// adopt nor be adopted.
pub const WORKTREE_SPEC: &str = "WORKTREE";

/// The review directory this identity should read and write.
///
/// For a named session that is the name's own id, and nothing else is
/// consulted. For a range it is the id of an existing review when this
/// spelling names the same work, with the join recorded; otherwise this
/// spelling's own id, with what it was opened as recorded so a later spelling
/// can find it.
pub fn resolve<C: ReviewCatalogue, G: Ancestry>(
    catalogue: &C,
    git: &G,
    opened_as: &ReviewIdentity,
) -> Result<String, EngineError> {
    let (base_sha, head_spec) = match opened_as {
        // A name is the identity. No alias, no scan, no git call — and no
        // adoption in either direction, because the reader has already said
        // which review this is.
        ReviewIdentity::Named(_) | ReviewIdentity::Remote(_) => {
            let id = match opened_as {
                ReviewIdentity::Named(name) => plan::review_id_named(name),
                ReviewIdentity::Remote(remote) => plan::review_id_remote(remote),
                ReviewIdentity::Range { .. } => unreachable!("matched above"),
            };
            if !catalogue.filed_reviews()?.iter().any(|r| r.id == id) {
                catalogue.file_identity(&id, opened_as)?;
            }
            return Ok(id);
        }
        ReviewIdentity::Range { base, head_spec } => (base.as_str(), head_spec.as_str()),
    };
    let id = plan::review_id(base_sha, head_spec);

    // A join already recorded. One file read, no scan and no git call.
    if let Some(target) = catalogue.alias_of(&id)? {
        return Ok(target);
    }

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
            catalogue.file_identity(&id, opened_as)?;
        }
        return Ok(id);
    }

    match adopt(&filed, base_sha, head_spec, git)? {
        Some(target) => {
            catalogue.file_alias(&id, &target)?;
            Ok(target)
        }
        None => {
            catalogue.file_identity(&id, opened_as)?;
            Ok(id)
        }
    }
}

/// The review this range should inherit, if any.
///
/// Cheapest test first: the base is a string compare and drops every review of
/// other work before a single git process starts.
fn adopt<G: Ancestry>(
    filed: &[FiledReview],
    base_sha: &str,
    head_spec: &str,
    git: &G,
) -> Result<Option<String>, EngineError> {
    let Some(head) = git.commit_of(head_spec)? else {
        return Ok(None);
    };

    // Where each filed review of this base sits relative to us: the same
    // commit, behind us, or ahead of us. A review on another line of history
    // is in none of them, which is what keeps two branches apart.
    let mut exact: Option<&str> = None;
    let mut behind: Vec<(&str, String)> = Vec::new();
    let mut ahead: Vec<(&str, String)> = Vec::new();

    for r in filed {
        // A named session is never adopted: its reader has said what it is.
        let Some(ReviewIdentity::Range {
            base: other_base,
            head_spec: other_spec,
        }) = r.opened_as.as_ref()
        else {
            continue;
        };
        if other_base != base_sha || other_spec == WORKTREE_SPEC {
            continue;
        }
        // A branch deleted since the review was filed cannot be placed.
        let Some(other_head) = git.commit_of(other_spec)? else {
            continue;
        };
        if other_head == head {
            exact = Some(&r.id);
            break;
        }
        if git.is_ancestor(&other_head, &head)? {
            behind.push((&r.id, other_head));
        } else if git.is_ancestor(&head, &other_head)? {
            ahead.push((&r.id, other_head));
        }
    }

    if let Some(id) = exact {
        return Ok(Some(id.to_string()));
    }
    // The newest review we have moved past, then the oldest that has moved
    // past us. The two sets cannot disagree: anything ahead of us is a
    // descendant of everything behind us, so only one set's own members can.
    if let Some(id) = extreme(git, &behind, Reach::Newest)? {
        return Ok(Some(id));
    }
    extreme(git, &ahead, Reach::Oldest)
}

/// Which end of a line of history to take.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    Newest,
    Oldest,
}

/// The one member of `set` that every other member reaches, or `None`.
///
/// **An ambiguous answer is no answer.** A merge head can descend from two
/// filed reviews that are not each other's ancestor, and there is no honest
/// reason to prefer one — so this declines and the caller files a fresh
/// review. Nothing is lost either way: both reviews keep their own marks under
/// their own ids. Choosing by scan order would have been arbitrary and silent.
fn extreme<G: Ancestry>(
    git: &G,
    set: &[(&str, String)],
    reach: Reach,
) -> Result<Option<String>, EngineError> {
    let Some((mut best, mut best_head)) = set.first().map(|(id, h)| (*id, h.as_str())) else {
        return Ok(None);
    };
    let mut best_at = 0;
    for (i, (id, head)) in set.iter().enumerate().skip(1) {
        let further = match reach {
            Reach::Newest => git.is_ancestor(best_head, head)?,
            Reach::Oldest => git.is_ancestor(head, best_head)?,
        };
        if further {
            (best, best_head, best_at) = (id, head, i);
        }
    }
    // One pass finds a candidate; this one proves it. Without it, two members
    // on different lines both survive the first pass and the answer depends on
    // the order they were read in.
    for (i, (_, head)) in set.iter().enumerate() {
        if i == best_at {
            continue;
        }
        let ordered = match reach {
            Reach::Newest => git.is_ancestor(head, best_head)?,
            Reach::Oldest => git.is_ancestor(best_head, head)?,
        };
        if !ordered {
            return Ok(None);
        }
    }
    Ok(Some(best.to_string()))
}
