//! Which review a range opens (issue 40).
//!
//! A review's id is a hash of the base sha and the head spec AS TYPED, so one
//! range typed two ways used to be two reviews. Adoption joins them: same
//! base, and one head reachable from the other, is one piece of work.
//!
//! Hermetic temp repositories and real `git` throughout — a fake git is
//! forbidden (ADR 0002, ADR 0020), and ancestry is exactly the question only
//! git can answer.

use differential_engine::plan;
use differential_engine::ports::{Ancestry, ReviewCatalogue, ReviewIdentity};
use differential_engine::review_identity::{WORKTREE_SPEC, resolve};
use differential_engine::store::FsReviewCatalogue;
use differential_testutil::TestRepo;

/// A repo with three commits on one line of history, plus a second branch off
/// the first: base, then `a`, then `b`; and `side` off `base`.
struct Line {
    r: TestRepo,
    base: String,
    a: String,
    b: String,
    side: String,
}

fn line() -> Line {
    let r = TestRepo::new();
    r.write("f.txt", b"one\n");
    let base = r.commit_all("base");
    r.write("f.txt", b"two\n");
    let a = r.commit_all("a");
    r.write("f.txt", b"three\n");
    let b = r.commit_all("b");

    r.git(&["checkout", "-q", "-b", "side", &base]);
    r.write("g.txt", b"side\n");
    let side = r.commit_all("side");
    r.git(&["checkout", "-q", "main"]);

    Line {
        r,
        base,
        a,
        b,
        side,
    }
}

fn catalogue(l: &Line) -> FsReviewCatalogue {
    FsReviewCatalogue::at(l.r.root.join(".git"))
}

fn range(base: &str, head_spec: &str) -> ReviewIdentity {
    ReviewIdentity::Range {
        base: base.to_string(),
        head_spec: head_spec.to_string(),
    }
}

#[test]
fn a_first_review_keeps_its_own_id_and_records_what_it_was_opened_as() {
    let l = line();
    let cat = catalogue(&l);
    let id = resolve(&cat, &l.r.repo(), &range(&l.base, &l.a)).unwrap();

    assert_eq!(id, plan::review_id(&l.base, &l.a), "nothing to adopt yet");
    let filed = cat.filed_reviews().unwrap();
    assert_eq!(filed.len(), 1);
    assert_eq!(
        filed[0].opened_as.as_ref().expect("identity is recorded"),
        &range(&l.base, &l.a)
    );
}

#[test]
fn two_spellings_of_one_commit_are_one_review() {
    let l = line();
    let cat = catalogue(&l);
    let git = l.r.repo();

    // Reviewed as a full sha, reopened as HEAD — which IS that sha.
    let first = resolve(&cat, &git, &range(&l.base, &l.b)).unwrap();
    let second = resolve(&cat, &git, &range(&l.base, "HEAD")).unwrap();
    assert_eq!(second, first, "same base, same commit, same review");

    // An abbreviated sha is a third spelling of the same thing.
    let short = resolve(&cat, &git, &range(&l.base, &l.b[..8])).unwrap();
    assert_eq!(short, first);
}

#[test]
fn a_review_carries_forward_when_the_head_moves_on() {
    let l = line();
    let cat = catalogue(&l);
    let git = l.r.repo();

    // Review at `a`, commit, then review at `b`. `a` is an ancestor of `b`,
    // so this is the same work read further along, not a new review.
    let first = resolve(&cat, &git, &range(&l.base, &l.a)).unwrap();
    let later = resolve(&cat, &git, &range(&l.base, &l.b)).unwrap();
    assert_eq!(later, first);

    // And backwards: an older sha reaches the same review.
    let l2 = line();
    let cat2 = catalogue(&l2);
    let git2 = l2.r.repo();
    let newest = resolve(&cat2, &git2, &range(&l2.base, &l2.b)).unwrap();
    assert_eq!(
        resolve(&cat2, &git2, &range(&l2.base, &l2.a)).unwrap(),
        newest
    );
}

#[test]
fn two_branches_off_one_base_stay_apart() {
    let l = line();
    let cat = catalogue(&l);
    let git = l.r.repo();

    let main = resolve(&cat, &git, &range(&l.base, &l.b)).unwrap();
    let side = resolve(&cat, &git, &range(&l.base, &l.side)).unwrap();
    assert_ne!(
        side, main,
        "neither head reaches the other, so the findings must never collide"
    );
}

#[test]
fn a_different_base_is_a_different_review() {
    let l = line();
    let cat = catalogue(&l);
    let git = l.r.repo();

    let from_base = resolve(&cat, &git, &range(&l.base, &l.b)).unwrap();
    let from_a = resolve(&cat, &git, &range(&l.a, &l.b)).unwrap();
    assert_ne!(from_a, from_base, "a different base is different work");
}

#[test]
fn uncommitted_work_never_adopts_and_is_never_adopted() {
    let l = line();
    let cat = catalogue(&l);
    let git = l.r.repo();

    let committed = resolve(&cat, &git, &range(&l.base, &l.b)).unwrap();
    let worktree = resolve(&cat, &git, &range(&l.base, WORKTREE_SPEC)).unwrap();
    assert_eq!(worktree, plan::review_id(&l.base, WORKTREE_SPEC));
    assert_ne!(worktree, committed, "a tree endpoint has no ancestry");

    // It records no identity, so it cannot be adopted later either.
    assert!(
        !cat.filed_reviews()
            .unwrap()
            .iter()
            .any(|r| r.opened_as == Some(range(&l.base, WORKTREE_SPEC))),
        "a worktree review must stay out of every scan"
    );
}

#[test]
fn an_adoption_is_recorded_and_answered_without_git() {
    let l = line();
    let cat = catalogue(&l);
    let git = l.r.repo();

    let first = resolve(&cat, &git, &range(&l.base, &l.b)).unwrap();
    let id = plan::review_id(&l.base, "HEAD");
    assert_eq!(resolve(&cat, &git, &range(&l.base, "HEAD")).unwrap(), first);

    // The join is on disk, so the second open is one file read.
    assert_eq!(cat.alias_of(&id).unwrap().as_deref(), Some(first.as_str()));
    // A redirect is a pointer, not a review: it must not enter a later scan.
    assert!(cat.filed_reviews().unwrap().iter().all(|r| r.id != id));
}

#[test]
fn a_candidate_whose_branch_is_gone_is_skipped_not_fatal() {
    let l = line();
    let cat = catalogue(&l);
    let git = l.r.repo();

    l.r.git(&["branch", "gone", &l.a]);
    let stale = resolve(&cat, &git, &range(&l.base, "gone")).unwrap();
    l.r.git(&["branch", "-D", "gone"]);

    // The stale candidate cannot be placed, so the scan passes over it.
    let fresh = resolve(&cat, &git, &range(&l.base, &l.b)).unwrap();
    assert_ne!(fresh, stale);
    assert_eq!(fresh, plan::review_id(&l.base, &l.b));
}

#[test]
fn git_answers_ancestry_the_way_adoption_reads_it() {
    // The one question adoption cannot ask without git, pinned directly.
    let l = line();
    let git = l.r.repo();

    assert!(git.is_ancestor(&l.a, &l.b).unwrap());
    assert!(!git.is_ancestor(&l.b, &l.a).unwrap());
    assert!(!git.is_ancestor(&l.side, &l.b).unwrap());
    assert_eq!(
        git.commit_of("HEAD").unwrap().as_deref(),
        Some(l.b.as_str())
    );
    assert_eq!(git.commit_of("no-such-ref").unwrap(), None);
}

#[test]
fn a_named_session_keys_on_the_name_and_nothing_else() {
    let l = line();
    let cat = catalogue(&l);
    let git = l.r.repo();
    let named = ReviewIdentity::Named("my-branch".into());

    let id = resolve(&cat, &git, &named).unwrap();
    assert_eq!(id, plan::review_id_named("my-branch"));

    // Same name, any range, and from the picker: one review.
    assert_eq!(resolve(&cat, &git, &named).unwrap(), id);
    assert_ne!(id, resolve(&cat, &git, &range(&l.base, &l.b)).unwrap());
    assert_ne!(
        id,
        resolve(&cat, &git, &ReviewIdentity::Named("other".into())).unwrap()
    );
}

#[test]
fn a_named_session_survives_a_rebase_of_either_endpoint() {
    let l = line();
    let cat = catalogue(&l);
    let named = ReviewIdentity::Named("my-branch".into());
    let before = resolve(&cat, &l.r.repo(), &named).unwrap();

    // Rewrite both endpoints: main moves, and the branch rebases onto it.
    l.r.git(&["checkout", "-q", "-b", "feature", &l.b]);
    l.r.write("mine.txt", b"alpha_value = 2\n");
    l.r.commit_all("feature work");
    l.r.git(&["checkout", "-q", "main"]);
    l.r.write("shared.txt", b"moved on\n");
    l.r.commit_all("main moves");
    l.r.git(&["checkout", "-q", "feature"]);
    l.r.git(&["rebase", "-q", "main"]);

    assert_eq!(
        resolve(&cat, &l.r.repo(), &named).unwrap(),
        before,
        "the name is the identity, so neither endpoint can strand it"
    );
}

#[test]
fn a_named_session_neither_adopts_nor_is_adopted() {
    let l = line();
    let cat = catalogue(&l);
    let git = l.r.repo();

    // A named session first, then the range it was read over.
    let named = resolve(&cat, &git, &ReviewIdentity::Named("mine".into())).unwrap();
    let ranged = resolve(&cat, &git, &range(&l.base, &l.a)).unwrap();
    assert_ne!(ranged, named, "a name is not a candidate for a range");

    // And a later spelling adopts the RANGE review, never the named one.
    assert_eq!(resolve(&cat, &git, &range(&l.base, &l.b)).unwrap(), ranged);
}
