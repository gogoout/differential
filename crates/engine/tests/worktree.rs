//! Uncommitted review sources (ADR 0017): index/worktree tree snapshots, and
//! the full pipeline — all four invariants — over the synthesized endpoints.

use differential_engine::config::Config;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_pipeline;
use differential_engine::ports::ObjectReader;
use differential_engine::ports::TreeResolver;
use differential_engine::schema::SourceKind;
use differential_engine::worktree::{index_tree, is_clean, worktree_tree};
use differential_testutil::{TestRepo, assert_all_ok};

/// base commit, then: a.txt staged, b.txt edited unstaged, new.txt untracked,
/// gone.txt deleted from the worktree only.
fn dirty_repo() -> (TestRepo, String) {
    let r = TestRepo::new();
    r.write("a.txt", b"alpha original line\n");
    r.write("b.txt", b"beta original line\n");
    r.write("gone.txt", b"short lived\n");
    let head = r.commit_all("base");

    r.write("a.txt", b"alpha STAGED change\n");
    r.git(&["add", "a.txt"]);
    r.write("b.txt", b"beta UNSTAGED change\n");
    r.write("new.txt", b"untracked newcomer\n");
    std::fs::remove_file(r.root.join("gone.txt")).unwrap();
    (r, head)
}

#[test]
fn index_tree_captures_staged_state_only() {
    let (r, head) = dirty_repo();
    let repo = r.repo();
    let tree = index_tree(&repo).unwrap();

    // Staged edit present; unstaged edit and untracked file absent; the
    // worktree-only deletion not staged, so the file is still there.
    assert_eq!(
        repo.blob(&tree, b"a.txt").unwrap().unwrap(),
        b"alpha STAGED change\n"
    );
    assert_eq!(
        repo.blob(&tree, b"b.txt").unwrap().unwrap(),
        b"beta original line\n"
    );
    assert!(repo.blob(&tree, b"new.txt").unwrap().is_none());
    assert!(repo.blob(&tree, b"gone.txt").unwrap().is_some());

    // The user's own index is untouched: staged state still lists a.txt only.
    let status = r.git(&["status", "--porcelain"]);
    assert!(status.contains("M  a.txt"), "index mutated? {status}");
    assert!(status.contains("?? new.txt"), "index mutated? {status}");
    let _ = head;
}

#[test]
fn worktree_tree_captures_everything_uncommitted() {
    let (r, _head) = dirty_repo();
    let repo = r.repo();
    let tree = worktree_tree(&repo).unwrap();

    assert_eq!(
        repo.blob(&tree, b"a.txt").unwrap().unwrap(),
        b"alpha STAGED change\n"
    );
    assert_eq!(
        repo.blob(&tree, b"b.txt").unwrap().unwrap(),
        b"beta UNSTAGED change\n"
    );
    assert_eq!(
        repo.blob(&tree, b"new.txt").unwrap().unwrap(),
        b"untracked newcomer\n"
    );
    assert!(repo.blob(&tree, b"gone.txt").unwrap().is_none());
}

/// The real proof: the whole pipeline — enumeration, classification and all
/// four invariants — runs unchanged over (commit, index-tree) and
/// (index-tree, worktree-tree).
#[test]
fn full_pipeline_passes_invariants_over_synthesized_trees() {
    let (r, head) = dirty_repo();
    let repo = r.repo();
    let index = index_tree(&repo).unwrap();
    let wt = worktree_tree(&repo).unwrap();

    for (base, head_rev, kind) in [
        (head.as_str(), index.as_str(), SourceKind::Staged),
        (index.as_str(), wt.as_str(), SourceKind::Worktree),
    ] {
        let out = run_pipeline(
            &repo,
            base,
            head_rev,
            kind,
            &Config::default(),
            &LanguageRegistry::builtin(),
            &differential_testutil::stub_readers(),
        )
        .unwrap();
        assert_all_ok(&out);
        let doc = out.document.unwrap();
        assert_eq!(doc.source.kind, kind);
        assert!(!doc.files.is_empty());
    }
}

#[test]
fn unmerged_index_is_a_clear_error() {
    let r = TestRepo::new();
    r.write("c.txt", b"base\n");
    r.commit_all("base");
    r.git(&["checkout", "-q", "-b", "side"]);
    r.write("c.txt", b"side version\n");
    r.commit_all("side");
    r.git(&["checkout", "-q", "main"]);
    r.write("c.txt", b"main version\n");
    r.commit_all("main edit");
    // Merge conflicts; git merge exits non-zero, so call it tolerantly.
    // Identity flags matter: without them git aborts BEFORE writing the
    // conflict entries on machines with no global user config (CI).
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=test", "-c", "user.email=t@example.invalid"])
        .args(["merge", "side"])
        .current_dir(&r.root)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "merge unexpectedly clean: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let err = index_tree(&r.repo()).unwrap_err();
    assert!(err.to_string().contains("unmerged"), "got: {err}");
}

/// A committed repo with nothing outstanding — the case every existing fixture
/// here skips, and the one the picker keys its checkbox off.
fn clean_repo() -> (TestRepo, String) {
    let r = TestRepo::new();
    r.write("a.txt", b"alpha original line\n");
    r.write("b.txt", b"beta original line\n");
    let head = r.commit_all("base");
    (r, head)
}

#[test]
fn is_clean_distinguishes_a_settled_worktree_from_a_dirty_one() {
    let (clean, _) = clean_repo();
    assert!(is_clean(&clean.repo()).unwrap());

    let (dirty, _) = dirty_repo();
    assert!(!is_clean(&dirty.repo()).unwrap());
}

/// Each kind of dirt on its own, so a detector that catches only some of them
/// fails here rather than silently hiding an option the reviewer needs.
#[test]
fn is_clean_catches_every_kind_of_uncommitted_change() {
    // Staged.
    let (r, _) = clean_repo();
    r.write("a.txt", b"staged edit\n");
    r.git(&["add", "a.txt"]);
    assert!(!is_clean(&r.repo()).unwrap(), "staged change");

    // Unstaged.
    let (r, _) = clean_repo();
    r.write("a.txt", b"unstaged edit\n");
    assert!(!is_clean(&r.repo()).unwrap(), "unstaged change");

    // Untracked but not ignored — the half `diff-index` cannot see, which is
    // why `untracked_paths` is part of the answer.
    let (r, _) = clean_repo();
    r.write("newcomer.txt", b"hello\n");
    assert!(!is_clean(&r.repo()).unwrap(), "untracked file");

    // Deleted from the worktree only.
    let (r, _) = clean_repo();
    std::fs::remove_file(r.root.join("a.txt")).unwrap();
    assert!(!is_clean(&r.repo()).unwrap(), "worktree deletion");

    // Ignored files are not dirt: a snapshot excludes them too.
    let (r, _) = clean_repo();
    r.write(".gitignore", b"junk/\n");
    r.git(&["add", ".gitignore"]);
    r.git(&["commit", "-q", "-m", "ignore junk"]);
    r.write("junk/thing.txt", b"noise\n");
    assert!(
        is_clean(&r.repo()).unwrap(),
        "ignored files are not changes"
    );
}

/// The identity the picker change rests on: with nothing outstanding, the
/// snapshot IS `HEAD`'s tree, so offering to include the worktree cannot
/// change the diff — it only costs the snapshot.
#[test]
fn a_clean_worktree_snapshots_to_exactly_the_head_tree() {
    let (r, _) = clean_repo();
    let repo = r.repo();
    let head_tree = repo.tree_of("HEAD").unwrap();

    assert_eq!(worktree_tree(&repo).unwrap(), head_tree);
    assert_eq!(index_tree(&repo).unwrap(), head_tree);
}
