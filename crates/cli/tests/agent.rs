//! `dfr agent` — the grouping model's read path (ADR 0022).
//!
//! The model reaches this through a subprocess, so the tests do too. What it
//! prints is the contract; anything it cannot answer must say so and exit 0,
//! because a non-zero exit reads to an agent as "the tool is broken" rather
//! than "there is no such class".

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

    let out = r.pipeline(&base, &head);
    let doc = out.document.expect("document");
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("doc.json");
    std::fs::write(&path, doc.to_json_pretty().unwrap()).unwrap();
    (r, dir, path)
}

fn agent(r: &TestRepo, doc: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_dfr"))
        .arg("agent")
        .arg("--doc")
        .arg(doc)
        .arg("--repo")
        .arg(r.repo().root())
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "dfr agent {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn classes_lists_every_class_with_its_graph() {
    let (r, _dir, doc) = document();
    let text = agent(&r, &doc, &["classes"]);

    assert!(text.contains("C0"), "{text}");
    assert!(text.contains("C1"), "{text}");
    assert!(
        text.contains("defines: WidgetCore"),
        "the index carries what each class introduces\n{text}"
    );
    assert!(
        text.contains("(WidgetCore)"),
        "and the symbol behind each edge\n{text}"
    );
}

#[test]
fn diff_shows_a_hunk_the_document_only_points_at() {
    let (r, _dir, doc) = document();
    // The document records positions, never text: this is the query that
    // re-reads the range, and the only reason a class can be checked at all.
    let text = agent(&r, &doc, &["diff", "C0"]);
    assert!(
        text.contains("+pub struct WidgetCore { pub retries: u32 }"),
        "{text}"
    );
    assert!(text.starts_with("--- h"), "each hunk names itself\n{text}");
}

#[test]
fn class_lists_every_member_not_just_the_exemplar() {
    let (r, _dir, doc) = document();
    let text = agent(&r, &doc, &["class", "C0"]);
    assert!(text.contains("hunks:"), "{text}");
    assert!(text.contains("(exemplar)"), "{text}");
    assert!(text.contains("src/a_core.txt"), "{text}");
}

#[test]
fn file_and_defines_find_the_same_class_from_two_directions() {
    let (r, _dir, doc) = document();
    let by_file = agent(&r, &doc, &["file", "src/a_core.txt"]);
    let by_symbol = agent(&r, &doc, &["defines", "WidgetCore"]);
    assert!(by_file.contains("C0"), "{by_file}");
    assert_eq!(
        by_file.lines().next(),
        by_symbol.lines().next(),
        "both reach the class that introduces WidgetCore"
    );
}

#[test]
fn an_unknown_id_says_so_and_still_exits_zero() {
    let (r, _dir, doc) = document();
    // Exit 0 with a plain sentence: an agent treats a non-zero exit as a
    // broken tool and stops asking, which is worse than a clear "no".
    assert_eq!(agent(&r, &doc, &["class", "C99"]), "no class C99\n");
    assert_eq!(agent(&r, &doc, &["diff", "h99"]), "no hunk or class h99\n");
    assert_eq!(
        agent(&r, &doc, &["defines", "Nothing"]),
        "no class defines Nothing\n"
    );
}
