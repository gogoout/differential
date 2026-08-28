//! `dfr agent` — the grouping model's read path (ADR 0022).
//!
//! The model reaches this through a subprocess, so the tests do too. What it
//! prints is the contract.
//!
//! One command, no sub-questions, no `--repo`: every answer comes from the
//! document, and diff text is `git diff`'s job.

use std::process::Command;

use differential_testutil::TestRepo;
use tempfile::TempDir;

/// A two-class change with one real dependency: `b_user` references the type
/// `a_core` introduces.
fn document() -> (TestRepo, TempDir, std::path::PathBuf) {
    let r = TestRepo::new();
    r.write("src/a_core.txt", b"placeholder\n");
    r.write("src/b_user.txt", b"placeholder\n");
    let base = r.commit_all("base");
    r.write(
        "src/a_core.txt",
        b"placeholder\npub struct WidgetCore { pub retries: u32 }\n",
    );
    r.write(
        "src/b_user.txt",
        b"placeholder\nlet core = WidgetCore { retries: 3 };\n",
    );
    let head = r.commit_all("head");
    let (dir, path) = write_doc(&r, &base, &head);
    (r, dir, path)
}

fn write_doc(r: &TestRepo, base: &str, head: &str) -> (TempDir, std::path::PathBuf) {
    let doc = r.pipeline(base, head).document.expect("document");
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("doc.json");
    std::fs::write(&path, doc.to_json_pretty().unwrap()).unwrap();
    (dir, path)
}

fn agent(doc: &std::path::Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_dfr"))
        .arg("agent")
        .arg("--doc")
        .arg(doc)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "dfr agent: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A change with a lockfile in it: generated content the noise tier folds and
/// the model is never asked to group.
fn document_with_generated() -> (TestRepo, TempDir, std::path::PathBuf) {
    let r = TestRepo::new();
    r.write("src/a.txt", b"placeholder\n");
    r.write("Cargo.lock", b"placeholder\n");
    let base = r.commit_all("base");
    r.write("src/a.txt", b"placeholder\npub struct WidgetCore;\n");
    r.write("Cargo.lock", b"placeholder\nchecksum = \"beefcafe\"\n");
    let head = r.commit_all("head");
    let (dir, path) = write_doc(&r, &base, &head);
    (r, dir, path)
}

#[test]
fn one_call_prints_every_class_with_its_graph() {
    let (_r, _dir, doc) = document();
    let text = agent(&doc);

    assert!(text.contains("C0"), "{text}");
    assert!(text.contains("C1"), "{text}");
    assert!(
        text.contains("defines: WidgetCore"),
        "what each class introduces\n{text}"
    );
    assert!(
        text.contains("(WidgetCore)"),
        "and the symbol behind each edge\n{text}"
    );
    assert!(
        text.contains("used by: C1"),
        "including the reverse edge, which a reader cannot derive from its own \
         entry\n{text}"
    );
}

#[test]
fn every_member_gets_a_location_not_just_the_exemplar() {
    let (_r, _dir, doc) = document();
    let text = agent(&doc);
    assert!(text.contains("(exemplar)"), "{text}");

    // The claim this output has to support: a class of N hunks is a claim about
    // N hunks, and the model can only check it by reading all N. So every
    // member gets a file and a line range — the two things a `git diff`
    // invocation needs.
    let members: Vec<&str> = text.lines().filter(|l| l.starts_with("  h")).collect();
    assert_eq!(members.len(), 2, "one line per hunk in the change\n{text}");
    for line in &members {
        assert!(line.contains(".txt"), "every member names its file\n{line}");
        assert!(line.contains("@@ -"), "and its line range\n{line}");
    }
}

#[test]
fn generated_content_is_not_offered_but_a_mixed_class_says_so() {
    let (_r, _dir, doc) = document_with_generated();
    let text = agent(&doc);

    // The model gets exactly what it is allowed to group. Naming a lockfile
    // class would be penalised by the audit, so the lockfile is not put in
    // front of it.
    assert!(text.contains("src/a.txt"), "{text}");
    assert!(
        !text.contains("Cargo.lock"),
        "wholly generated, so not offered\n{text}"
    );
    // And nothing offered here has a generated file in it, so no class claims
    // one — the marker would mean nothing if it appeared unearned.
    assert!(!text.contains("generated:"), "{text}");
}

#[test]
fn an_empty_change_says_so_rather_than_printing_nothing() {
    // A blank reply reads to an agent as a broken tool. Exit 0 and a sentence.
    let r = TestRepo::new();
    r.write("src/a.txt", b"placeholder\n");
    let base = r.commit_all("base");
    let (_dir, doc) = write_doc(&r, &base, &base);
    assert_eq!(agent(&doc), "no classes\n");
}
