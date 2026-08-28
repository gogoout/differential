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

    let doc = r.pipeline(&base, &head).document.expect("document");
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("doc.json");
    std::fs::write(&path, doc.to_json_pretty().unwrap()).unwrap();
    (r, dir, path)
}

#[test]
fn generated_content_is_not_offered_but_is_still_reachable() {
    let (r, _dir, doc) = document_with_generated();

    // Asked without ids, the model gets exactly what it is allowed to group.
    // Handing it a lockfile would be bytes it must read and cannot use, and a
    // class id it would be penalised for naming.
    let index = agent(&r, &doc, &["classes"]);
    assert!(index.contains("src/a.txt"), "{index}");
    assert!(
        !index.contains("Cargo.lock"),
        "generated, so not offered\n{index}"
    );

    let whole = agent(&r, &doc, &["diff"]);
    assert!(whole.contains("+pub struct WidgetCore;"), "{whole}");
    assert!(
        !whole.contains("beefcafe"),
        "generated, so not served\n{whole}"
    );

    // The noise tier folds; it never hides. An explicit id still answers.
    let named = agent(&r, &doc, &["file", "Cargo.lock"]);
    assert!(named.contains("Cargo.lock"), "{named}");
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
fn several_ids_come_back_in_one_call() {
    let (r, _dir, doc) = document();
    // The whole point: one round trip instead of one per class. The batch
    // enumerates the range once, however many ids it carries.
    let batched = agent(&r, &doc, &["diff", "C0", "C1"]);
    let one_by_one = format!(
        "{}{}",
        agent(&r, &doc, &["diff", "C0"]),
        agent(&r, &doc, &["diff", "C1"])
    );
    assert_eq!(batched, one_by_one);
    assert_eq!(batched.matches("--- h").count(), 2);

    let classes = agent(&r, &doc, &["class", "C0", "C1"]);
    assert!(classes.contains("C0"), "{classes}");
    assert!(classes.contains("C1"), "{classes}");
}

#[test]
fn no_ids_means_everything() {
    let (r, _dir, doc) = document();
    // Two calls should be enough for any change: the shape, then the lot. The
    // engine holds every hunk already, so making a model rebuild that one id at
    // a time is round trips for nothing.
    let all = agent(&r, &doc, &["diff"]);
    let named = agent(&r, &doc, &["diff", "C0", "C1"]);
    assert_eq!(all, named, "no ids is every hunk, in canonical order");

    let classes = agent(&r, &doc, &["class"]);
    assert!(
        classes.contains("C0") && classes.contains("C1"),
        "{classes}"
    );
}

#[test]
fn a_cursor_resumes_exactly_where_it_stopped() {
    let (r, _dir, doc) = document();
    let whole = agent(&r, &doc, &["diff"]);

    // `--after` is exclusive: the named hunk is what you already have.
    let rest = agent(&r, &doc, &["diff", "--after", "h0"]);
    assert!(!rest.contains("--- h0"), "{rest}");
    assert!(rest.contains("--- h1"), "{rest}");
    assert_eq!(
        format!("{}{rest}", &whole[..whole.find("--- h1").unwrap()]),
        whole,
        "the two halves rejoin into exactly the whole"
    );

    // A cursor on the last hunk is finished, not broken. An empty reply to a
    // legitimate continue reads as a failure.
    assert_eq!(
        agent(&r, &doc, &["diff", "--after", "h1"]),
        "no more hunks: that was the end of the list\n"
    );
    assert_eq!(
        agent(&r, &doc, &["diff", "--after", "h9"]),
        "h9 is not in this list\n"
    );

    // The cursor walks the list the ids named, not the whole change.
    let named = agent(&r, &doc, &["diff", "C0", "C1", "--after", "h0"]);
    assert_eq!(named, rest);
}

/// Enough diff text to pass the reply cap: 40 files of roughly 10KB each.
fn big_document() -> (TestRepo, TempDir, std::path::PathBuf) {
    let r = TestRepo::new();
    for i in 0..40 {
        r.write(&format!("src/f{i}.txt"), b"placeholder\n");
    }
    let base = r.commit_all("base");
    for i in 0..40 {
        let mut body = b"placeholder\n".to_vec();
        for line in 0..120 {
            body.extend_from_slice(
                format!("let value_{i}_{line} = compute({});\n", "x".repeat(60)).as_bytes(),
            );
        }
        r.write(&format!("src/f{i}.txt"), &body);
    }
    let head = r.commit_all("head");

    let doc = r.pipeline(&base, &head).document.expect("document");
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("doc.json");
    std::fs::write(&path, doc.to_json_pretty().unwrap()).unwrap();
    (r, dir, path)
}

#[test]
fn a_reply_too_large_ends_with_the_command_that_continues_it() {
    let (r, _dir, doc) = big_document();

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut replies = 0;
    loop {
        let mut args = vec!["diff".to_string()];
        if let Some(c) = &cursor {
            args.push("--after".to_string());
            args.push(c.clone());
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = agent(&r, &doc, &refs);
        replies += 1;
        assert!(replies < 20, "the cursor must terminate");

        for line in out.lines().filter(|l| l.starts_with("--- h")) {
            seen.push(line.split_whitespace().nth(1).unwrap().to_string());
        }
        match out.lines().find(|l| l.contains("diff --after ")) {
            Some(line) => cursor = Some(line.rsplit(' ').next().unwrap().to_string()),
            None => break,
        }
    }

    assert!(replies > 1, "the fixture must actually exceed the cap");
    // Nothing dropped, nothing repeated: the cap bounds a reply, never the
    // change. That is the failure the old prompt cap had.
    assert_eq!(seen.len(), 40, "every hunk arrived exactly once");
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), seen.len(), "no hunk arrived twice");
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
