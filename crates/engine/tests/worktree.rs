//! Uncommitted review sources (ADR 0017): index/worktree tree snapshots, and
//! the full pipeline — all four invariants — over the synthesized endpoints.

mod common;

use common::{TestRepo, assert_all_ok};
use differential_engine::config::Config;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_pipeline;
use differential_engine::schema::SourceKind;
use differential_engine::worktree::{index_tree, worktree_tree};

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
