//! The verify stage is separate, and the split is the write boundary.
//!
//! `run_pipeline` reads. `verify` writes. These tests assert both halves of
//! that sentence, because the type system proves only one of them: the bound
//! list keeps the pipeline from writing, but nothing keeps `verify` honest
//! about what it folds back into the document.

use differential_testutil::TestRepo;

fn repo_with_a_change() -> (TestRepo, String, String) {
    let r = TestRepo::new();
    r.write("src/a.txt", b"one\ntwo\nthree\n");
    r.write("src/b.txt", b"alpha\n");
    let base = r.commit_all("base");
    r.write("src/a.txt", b"one\nTWO\nthree\n");
    r.write("src/c.txt", b"new\n");
    let head = r.commit_all("head");
    (r, base, head)
}

/// The claim the bound list makes, checked against a real repository.
///
/// A git double would be the obvious way to assert this and is forbidden
/// (ADR 0020), so the odb is counted instead. The type system carries the rest:
/// `run_pipeline` names no write port, so it cannot reach one.
#[test]
fn the_core_pipeline_writes_no_object() {
    let (r, base, head) = repo_with_a_change();

    let before = r.loose_object_count();
    let out = r.pipeline_read_only(&base, &head);
    let after = r.loose_object_count();

    assert_eq!(before, after, "the core pipeline wrote to the odb");
    assert!(out.document.is_some(), "invariants 1 and 2 passed");
}

/// The other half, and it is not what it looks like: **a passing verify adds no
/// new object.**
///
/// It calls `hash-object -w` for every reconstructed file and then
/// `write-tree`. Every one of those objects already exists, because the
/// reconstruction equalling head is exactly what invariant 3 asserts. New
/// objects appear only when the assertion is about to fail.
///
/// So the cost of the verify stage is subprocesses, not repository growth. That
/// is worth pinning, because the opposite is the obvious assumption.
#[test]
fn a_passing_verify_adds_no_new_object() {
    let (r, base, head) = repo_with_a_change();
    let mut out = r.pipeline_read_only(&base, &head);

    let before = r.loose_object_count();
    differential_engine::verify(&r.repo(), &mut out).unwrap();
    let after = r.loose_object_count();

    assert!(out.report.all_ok(), "{:#?}", out.report);
    assert_eq!(
        before, after,
        "everything verify writes is already in the head tree"
    );
}

/// The hazard the split introduces: a report that never ran the tree half must
/// not read as a pass.
#[test]
fn an_unverified_run_is_fidelity_ok_but_not_all_ok() {
    let (r, base, head) = repo_with_a_change();
    let out = r.pipeline_read_only(&base, &head);

    assert!(out.report.fidelity_ok(), "invariants 1 and 2 held");
    assert!(!out.report.all_ok(), "invariants 3 and 4 never ran");
    assert!(out.report.tree.is_none());
}

/// `generator.stages` is what a consumer must consult, so the two tree fields
/// have to be legible as "did not run" rather than as a failure.
#[test]
fn an_unverified_document_says_the_stage_was_skipped() {
    let (r, base, head) = repo_with_a_change();
    let out = r.pipeline_read_only(&base, &head);
    let doc = out.document.unwrap();

    assert!(!doc.generator.stages.iter().any(|s| s == "verify"));
    assert_eq!(doc.audit.tree_assertion, "skipped");
    assert_eq!(doc.audit.recount, 0);
    // Invariants 1 and 2 did run, so their two fields are real.
    assert_eq!(doc.audit.applier_exact, "2/2");
    assert_eq!(doc.audit.hunks_carried, 2);
}

/// And after verify, the same two fields carry an answer.
#[test]
fn verify_folds_its_result_into_the_document() {
    let (r, base, head) = repo_with_a_change();
    let mut out = r.pipeline_read_only(&base, &head);
    differential_engine::verify(&r.repo(), &mut out).unwrap();

    assert!(out.report.all_ok(), "{:#?}", out.report);
    let doc = out.document.unwrap();
    assert!(doc.generator.stages.iter().any(|s| s == "verify"));
    assert_eq!(doc.audit.tree_assertion, "pass");
    assert_eq!(doc.audit.recount, doc.stats.hunks);
}

/// Running it twice must not list the stage twice.
#[test]
fn verify_is_idempotent_in_the_stage_list() {
    let (r, base, head) = repo_with_a_change();
    let mut out = r.pipeline_read_only(&base, &head);
    differential_engine::verify(&r.repo(), &mut out).unwrap();
    differential_engine::verify(&r.repo(), &mut out).unwrap();

    let doc = out.document.unwrap();
    assert_eq!(
        doc.generator
            .stages
            .iter()
            .filter(|s| *s == "verify")
            .count(),
        1
    );
}
