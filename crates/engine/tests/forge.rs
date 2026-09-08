//! The forge consumer's domain (ADR 0029): where a fetched thread lands,
//! which findings a publish may send, and how the session keeps both apart.

use differential_engine::config::Config;
use differential_engine::forge::{self, RemoteComment, RemoteThread, Request};
use differential_engine::grouping::GroupingOptions;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_grouped_pipeline;
use differential_engine::plan::{self, ReviewSource};
use differential_engine::ports::{ReviewCatalogue, ReviewIdentity, ReviewStore};
use differential_engine::review_identity::resolve;
use differential_engine::review_state::Lines;
use differential_engine::schema::{self, Remote};
use differential_engine::store::{
    FsArtefactStore, FsGroupingCache, FsReviewCatalogue, FsReviewStore,
};
use differential_engine::{FsReviewSession, ReviewSession};
use differential_testutil::{FakeBackend, TestRepo, json_group};

fn focus_all_backend() -> FakeBackend {
    FakeBackend::new("fake", |ids| {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Everything", "focus", &refs)
        )
    })
}

/// Ten lines; the head changes line 3 and line 8, which `-U0` keeps as two
/// hunks with unchanged lines between them.
fn two_hunk_repo() -> (TestRepo, String, String) {
    let r = TestRepo::new();
    let lines: Vec<String> = (1..=10).map(|i| format!("line_{i} = {i}")).collect();
    r.write("src/lib.rs", format!("{}\n", lines.join("\n")).as_bytes());
    let base = r.commit_all("base");
    let mut changed = lines.clone();
    changed[2] = "line_3 = 300".to_string();
    changed[7] = "line_8 = 800".to_string();
    r.write("src/lib.rs", format!("{}\n", changed.join("\n")).as_bytes());
    let head = r.commit_all("head");
    (r, base, head)
}

fn doc_and_view(
    r: &TestRepo,
    base: &str,
    head: &str,
) -> (schema::PlanDocument, differential_engine::model::DiffView) {
    let backend = focus_all_backend();
    let out = run_grouped_pipeline(
        &r.repo(),
        &ReviewSource::range(base.to_string(), head.to_string(), head.to_string()),
        &Config::default(),
        &LanguageRegistry::builtin(),
        &differential_testutil::stub_readers(),
        &GroupingOptions {
            backend: &backend,
            cache: &FsGroupingCache::disabled(),
            artefacts: &FsArtefactStore::disabled(),
            fetch: "dfr",
            progress: None,
        },
    )
    .unwrap();
    (out.document.unwrap(), out.view)
}

fn thread(id: &str, path: &str, side: &str, line: Option<u32>) -> RemoteThread {
    RemoteThread {
        id: id.to_string(),
        resolved: false,
        outdated: line.is_none(),
        path: path.to_string(),
        side: side.to_string(),
        line,
        start_line: None,
        line_text: None,
        anchor: None,
        comments: vec![RemoteComment {
            id: format!("{id}-root"),
            author: "alice".into(),
            created: "2026-09-03T20:53:12Z".into(),
            body: "why?".into(),
            reply_to: None,
            finding: None,
        }],
    }
}

fn hunk_holding(doc: &schema::PlanDocument, new_line: u32) -> &schema::HunkEntry {
    doc.hunks
        .iter()
        .find(|h| new_line >= h.new_start && new_line < h.new_start + h.new_count.max(1))
        .expect("a hunk holds the line")
}

// ------------------------------------------------------------------ placing

#[test]
fn a_thread_on_a_changed_line_lands_exactly_with_the_hunks_own_text() {
    let (r, base, head) = two_hunk_repo();
    let (doc, view) = doc_and_view(&r, &base, &head);
    let mut t = thread("t1", "src/lib.rs", "new", Some(3));
    forge::place(&doc, &view, &mut t);
    let a = t.anchor.expect("placed");
    assert_eq!(a.hunk_digest, hunk_holding(&doc, 3).digest);
    assert_eq!((a.line, a.end_line, a.offset, a.span), (3, 3, 0, 0));
    // From the hunk's bytes, not from the forge: `reanchor` matches on these.
    assert_eq!(a.line_text, "line_3 = 300");
}

#[test]
fn a_multi_line_thread_spans_from_its_start_line() {
    let (r, base, head) = two_hunk_repo();
    let (doc, view) = doc_and_view(&r, &base, &head);
    let mut t = thread("t1", "src/lib.rs", "new", Some(8));
    t.start_line = Some(6);
    t.line_text = Some("line_8 = 800".into());
    forge::place(&doc, &view, &mut t);
    let a = t.anchor.expect("placed");
    assert_eq!(a.hunk_digest, hunk_holding(&doc, 8).digest);
    assert_eq!((a.line, a.end_line), (6, 8));
    // Line 6 is context: two above the hunk that starts at 8.
    assert_eq!((a.offset, a.span), (-2, 2));
    assert_eq!(a.end_line_text, "line_8 = 800");
}

#[test]
fn a_thread_on_a_context_line_lands_on_the_nearest_hunk_at_a_signed_offset() {
    let (r, base, head) = two_hunk_repo();
    let (doc, view) = doc_and_view(&r, &base, &head);
    // Line 5 is unchanged: two below the hunk at 3, three above the hunk at 8.
    let mut t = thread("t1", "src/lib.rs", "new", Some(5));
    t.line_text = Some("line_5 = 5".into());
    forge::place(&doc, &view, &mut t);
    let a = t.anchor.expect("placed");
    assert_eq!(a.hunk_digest, hunk_holding(&doc, 3).digest);
    assert_eq!((a.line, a.offset), (5, 2));
    // Nothing in the hunk says what line 5 is, so the forge's text stands.
    assert_eq!(a.line_text, "line_5 = 5");
}

#[test]
fn an_outdated_thread_is_found_by_its_text_or_left_unplaced() {
    let (r, base, head) = two_hunk_repo();
    let (doc, view) = doc_and_view(&r, &base, &head);

    let mut found = thread("t1", "src/lib.rs", "new", None);
    found.line_text = Some("line_8 = 800".into());
    forge::place(&doc, &view, &mut found);
    let a = found.anchor.expect("found by content");
    assert_eq!(a.hunk_digest, hunk_holding(&doc, 8).digest);
    assert_eq!(a.line, 8);

    // The old side is searched too, and the anchor moves to where it was found.
    let mut old_side = thread("t2", "src/lib.rs", "new", None);
    old_side.line_text = Some("line_3 = 3".into());
    forge::place(&doc, &view, &mut old_side);
    assert_eq!(
        old_side.anchor.as_ref().map(|a| a.side.as_str()),
        Some("old")
    );

    let mut gone = thread("t3", "src/lib.rs", "new", None);
    gone.line_text = Some("nothing like this".into());
    forge::place(&doc, &view, &mut gone);
    assert!(gone.anchor.is_none(), "counted, not drawn");

    let mut other_file = thread("t4", "elsewhere.rs", "new", Some(3));
    forge::place(&doc, &view, &mut other_file);
    assert!(other_file.anchor.is_none());
}

// --------------------------------------------------------------- publishing

fn session(r: &TestRepo, base: &str, head: &str, dir: &std::path::Path) -> FsReviewSession {
    let (doc, view) = doc_and_view(r, base, head);
    ReviewSession::open(FsReviewStore::at(dir.to_path_buf()).unwrap(), doc, view).unwrap()
}

fn lines(side: &str, start: u32, end: u32) -> Lines {
    Lines {
        side: side.into(),
        start,
        end,
        start_text: String::new(),
        end_text: String::new(),
    }
}

#[test]
fn a_publish_sends_open_unpublished_findings_inside_the_diff_and_names_the_rest() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    let h3 = s.doc().hunks.iter().position(|h| h.new_start == 3).unwrap();

    // On the change itself, two lines of context away, and far outside.
    let on_change = s
        .add_finding(h3, None, "on the change".into())
        .unwrap()
        .id
        .clone();
    let near = s
        .add_finding(h3, Some(lines("new", 5, 5)), "two below".into())
        .unwrap()
        .id
        .clone();
    let far = s
        .add_finding(h3, Some(lines("new", 40, 41)), "far away".into())
        .unwrap()
        .id
        .clone();
    s.set_threads(vec![thread("t1", "src/lib.rs", "new", Some(8))])
        .unwrap();
    let reply = s.add_reply("t1", "agreed".into()).unwrap().id.clone();

    let plan = s.publish_plan(forge::ForgeKind::Github);
    let sent: Vec<&str> = plan
        .batch
        .comments
        .iter()
        .map(|c| c.finding.as_str())
        .collect();
    assert_eq!(sent, vec![on_change.as_str(), near.as_str()]);
    assert_eq!(plan.batch.replies.len(), 1);
    assert_eq!(plan.batch.replies[0].finding, reply);
    assert_eq!(plan.batch.replies[0].thread, "t1");
    assert_eq!(plan.batch.replies[0].root_comment, "t1-root");
    assert_eq!(plan.excluded.len(), 1);
    assert_eq!(plan.excluded[0].finding, far);
    assert_eq!(plan.excluded[0].lines, "40-41");
    assert!(plan.excluded[0].reason.contains("outside"));

    let c = &plan.batch.comments[0];
    assert_eq!(
        (c.path.as_str(), c.side.as_str(), c.line, c.start_line),
        ("src/lib.rs", "new", 3, None)
    );
    assert!(c.old_path.is_none());

    // Publishing records the upstream address; a second plan sends nothing.
    let published: Vec<forge::Published> = plan
        .batch
        .comments
        .iter()
        .map(|c| forge::Published {
            finding: c.finding.clone(),
            thread: format!("thread-of-{}", c.finding),
            comment: format!("comment-of-{}", c.finding),
            url: None,
        })
        .chain(plan.batch.replies.iter().map(|r| forge::Published {
            finding: r.finding.clone(),
            thread: r.thread.clone(),
            comment: "reply-comment".into(),
            url: None,
        }))
        .collect();
    assert_eq!(s.mark_published(&published).unwrap(), 3);
    let again = s.publish_plan(forge::ForgeKind::Github);
    assert!(again.batch.is_empty());
    // Still excluded, still reported: it never left.
    assert_eq!(again.excluded.len(), 1);

    // `y` means "not yet on the request".
    let summary = s.findings_summary();
    assert!(summary.contains("far away"));
    assert!(!summary.contains("on the change"));
    assert!(!summary.contains("agreed"));
}

#[test]
fn a_reply_whose_thread_is_gone_is_excluded_not_sent_as_a_comment() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    s.set_threads(vec![thread("t1", "src/lib.rs", "new", Some(8))])
        .unwrap();
    s.add_reply("t1", "agreed".into()).unwrap();
    // The forge dropped the thread before the reply went up.
    s.set_threads(vec![]).unwrap();
    let plan = s.publish_plan(forge::ForgeKind::Github);
    assert!(plan.batch.is_empty());
    assert_eq!(plan.excluded.len(), 1);
    assert!(plan.excluded[0].reason.contains("thread"));
}

#[test]
fn a_published_finding_hides_behind_its_fetched_twin() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    let h3 = s.doc().hunks.iter().position(|h| h.new_start == 3).unwrap();
    let id = s.add_finding(h3, None, "mine".into()).unwrap().id.clone();
    s.mark_published(&[forge::Published {
        finding: id.clone(),
        thread: "T".into(),
        comment: "C".into(),
        url: None,
    }])
    .unwrap();
    let f = s.findings().iter().find(|f| f.id == id).unwrap().clone();
    assert!(!s.is_twinned(&f), "not fetched yet: the note still shows");
    let mut fetched = thread("T", "src/lib.rs", "new", Some(3));
    fetched.comments[0].id = "C".into();
    s.set_threads(vec![fetched]).unwrap();
    assert!(s.is_twinned(&f));
}

// -------------------------------------------------------------- persistence

#[test]
fn threads_persist_beside_findings_and_are_placed_again_on_open() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    {
        let mut s = session(&r, &base, &head, tmp.path());
        s.set_threads(vec![thread("t1", "src/lib.rs", "new", Some(3))])
            .unwrap();
        assert!(s.set_thread_resolved("t1", true).unwrap());
        assert!(!s.set_thread_resolved("nope", true).unwrap());
    }
    assert!(tmp.path().join("comments.jsonl").exists());
    assert!(
        !tmp.path().join("findings.jsonl").exists() || {
            // Findings were saved (empty) on open; either way the forge never
            // wrote into them.
            FsReviewStore::at(tmp.path().to_path_buf())
                .unwrap()
                .load_findings()
                .unwrap()
                .is_empty()
        }
    );
    let s = session(&r, &base, &head, tmp.path());
    assert_eq!(s.threads().len(), 1);
    assert!(s.threads()[0].resolved);
    assert!(s.threads()[0].anchor.is_some(), "placed on open");
}

#[test]
fn a_reply_draft_sits_where_its_thread_does() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    s.set_threads(vec![thread("t1", "src/lib.rs", "new", Some(8))])
        .unwrap();
    let f = s.add_reply("t1", "agreed".into()).unwrap().clone();
    assert_eq!(f.reply_to.as_deref(), Some("t1"));
    assert_eq!(f.anchor, s.thread("t1").unwrap().anchor.clone().unwrap());
    assert!(s.add_reply("missing", "x".into()).is_err());
}

// ----------------------------------------------------------------- identity

fn request() -> Request {
    Request {
        kind: forge::ForgeKind::Github,
        project: "owner/repo".into(),
        id: "123".into(),
        base_ref: "main".into(),
        base_tip: "b".repeat(40),
        head: "h".repeat(40),
        merge_base: None,
        url: "https://example.invalid/pull/123".into(),
    }
}

#[test]
fn a_request_is_a_review_identity_keyed_on_the_request_alone() {
    let req = request();
    let remote = Remote {
        forge: "github".into(),
        project: "owner/repo".into(),
        id: "123".into(),
    };
    assert_eq!(req.remote(), remote);
    assert_eq!(req.identity(), ReviewIdentity::Remote(remote.clone()));
    assert_eq!(
        req.fetch_hint("origin"),
        "git fetch origin main pull/123/head"
    );

    // Its own space: not a range's id, not a name's.
    let id = plan::review_id_remote(&remote);
    assert_ne!(id, plan::review_id_named("github\u{0}owner/repo\u{0}123"));
    assert_ne!(
        id,
        plan::review_id_remote(&Remote {
            id: "124".into(),
            ..remote.clone()
        })
    );
    assert_ne!(
        id,
        plan::review_id_remote(&Remote {
            forge: "gitlab".into(),
            ..remote
        })
    );
}

#[test]
fn a_request_review_is_filed_once_and_found_again_without_git() {
    let r = TestRepo::new();
    r.write("f.txt", b"one\n");
    r.commit_all("base");
    let cat = FsReviewCatalogue::at(r.root.join(".git"));
    let identity = request().identity();

    let first = resolve(&cat, &r.repo(), &identity).unwrap();
    let again = resolve(&cat, &r.repo(), &identity).unwrap();
    assert_eq!(first, again);
    assert_eq!(first, plan::review_id_remote(&request().remote()));

    let filed = cat.filed_reviews().unwrap();
    assert_eq!(filed.len(), 1);
    assert_eq!(
        filed[0].opened_as,
        Some(identity),
        "identity.json round-trips"
    );
}

#[test]
fn a_request_source_writes_the_remote_into_the_document() {
    let (r, base, head) = two_hunk_repo();
    let req = request();
    let source = ReviewSource::request(
        base.clone(),
        head.clone(),
        req.kind.source_kind(),
        req.remote(),
    );
    assert_eq!(source.head_spec, head);
    let out = differential_engine::run_pipeline(
        &r.repo(),
        &source,
        &Config::default(),
        &LanguageRegistry::builtin(),
        &differential_testutil::stub_readers(),
    )
    .unwrap();
    let doc = out.document.unwrap();
    assert_eq!(doc.source.kind, schema::SourceKind::Pr);
    assert_eq!(doc.source.remote, Some(req.remote()));
    assert!(forge::head_matches(
        &Request {
            head: head.clone(),
            ..req
        },
        &doc.source.head
    ));
}

// ------------------------------------------------------------- idempotency

#[test]
fn a_published_body_carries_its_finding_and_gives_it_back() {
    let sent = forge::with_marker("why three?\n", "abc123");
    assert_eq!(sent, "why three?\n\n<!-- differential:finding abc123 -->");
    assert_eq!(
        forge::strip_marker(&sent),
        ("why three?".to_string(), Some("abc123".to_string()))
    );
    // As GitHub gives it back: reflowed line endings, nothing after.
    assert_eq!(
        forge::strip_marker("why three?\r\n\r\n<!-- differential:finding abc123 -->\r\n"),
        ("why three?".to_string(), Some("abc123".to_string()))
    );
    assert_eq!(forge::strip_marker("plain"), ("plain".to_string(), None));
}

#[test]
fn a_fetch_reconciles_a_finding_the_forge_already_carries() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    let h3 = s.doc().hunks.iter().position(|h| h.new_start == 3).unwrap();
    let id = s
        .add_finding(h3, None, "on the change".into())
        .unwrap()
        .id
        .clone();

    // The publish's answer was lost: nothing was marked. The plan would send
    // it again — until the threads say it is there.
    assert_eq!(s.publish_plan(forge::ForgeKind::Github).batch.len(), 1);
    let mut t = thread("T1", "src/lib.rs", "new", Some(3));
    t.comments[0].id = "C1".into();
    t.comments[0].finding = Some(id.clone());
    t.comments[0].body = "on the change".into();
    assert_eq!(s.set_threads(vec![t]).unwrap(), 1, "one reconciled");
    let f = s.findings().iter().find(|f| f.id == id).unwrap();
    assert_eq!(
        f.upstream
            .as_ref()
            .map(|u| (u.thread.as_str(), u.comment.as_str())),
        Some(("T1", "C1"))
    );
    assert!(s.is_twinned(f));
    assert!(
        s.publish_plan(forge::ForgeKind::Github).batch.is_empty(),
        "nothing sent twice"
    );
    assert_eq!(s.findings_summary().trim(), "(no open findings)");
    // A second fetch has nothing left to reconcile.
    assert_eq!(s.set_threads(s.threads().to_vec()).unwrap(), 0);
}

#[test]
fn the_batch_sends_bodies_with_their_markers() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    let h3 = s.doc().hunks.iter().position(|h| h.new_start == 3).unwrap();
    let id = s
        .add_finding(h3, None, "on the change".into())
        .unwrap()
        .id
        .clone();
    let plan = s.publish_plan(forge::ForgeKind::Github);
    assert_eq!(
        plan.batch.comments[0].body,
        forge::with_marker("on the change", &id)
    );
}

#[test]
fn an_unmarked_reply_by_the_reader_heals_its_draft_and_the_side_is_checked() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    let h3 = s.doc().hunks.iter().position(|h| h.new_start == 3).unwrap();

    // Two notes on line 3, one per side, the same words; and a reply draft.
    s.set_threads(vec![thread("T1", "src/lib.rs", "new", Some(8))])
        .unwrap();
    let new_side = s
        .add_finding(h3, Some(lines("new", 3, 3)), "same words".into())
        .unwrap()
        .id
        .clone();
    // A trailing newline: the same words once normalised, and a different
    // finding id — the id hashes the body, and the two would otherwise be one.
    let old_side = s
        .add_finding(h3, Some(lines("old", 3, 3)), "same words\n".into())
        .unwrap()
        .id
        .clone();
    let reply = s
        .add_reply("T1", "agreed".into())
        .map(|f| f.id.clone())
        .unwrap();

    s.set_me("me".into());
    // The forge holds: a comment by me on the OLD side of line 3, and my reply
    // under T1 — neither with a marker.
    let mut mine = thread("M1", "src/lib.rs", "old", Some(3));
    mine.comments[0].author = "me".into();
    mine.comments[0].body = "same words".into();
    let mut t1 = thread("T1", "src/lib.rs", "new", Some(8));
    t1.comments.push(RemoteComment {
        id: "T1-mine".into(),
        author: "me".into(),
        created: "2026-09-07T10:00:00Z".into(),
        body: "agreed".into(),
        reply_to: Some("T1-root".into()),
        finding: None,
    });
    assert_eq!(s.set_threads(vec![mine, t1]).unwrap(), 2);

    let by = |id: &str| s.findings().iter().find(|f| f.id == id).unwrap();
    assert!(
        by(&new_side).upstream.is_none(),
        "the other side does not heal"
    );
    assert_eq!(
        by(&old_side).upstream.as_ref().map(|u| u.comment.as_str()),
        Some("M1-root")
    );
    assert_eq!(
        by(&reply)
            .upstream
            .as_ref()
            .map(|u| (u.thread.as_str(), u.comment.as_str())),
        Some(("T1", "T1-mine"))
    );
    // Not by anyone else.
    let mut theirs = thread("M2", "src/lib.rs", "new", Some(3));
    theirs.comments[0].body = "same words".into();
    assert_eq!(s.set_threads(vec![theirs]).unwrap(), 0);
}

#[test]
fn whose_a_comment_is_is_the_sessions_call() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    let h3 = s.doc().hunks.iter().position(|h| h.new_start == 3).unwrap();

    // Alice's thread with my reply under it; my own unmarked thread; a
    // published note whose twin is not fetched.
    let mut t1 = thread("T1", "src/lib.rs", "new", Some(3));
    t1.comments.push(RemoteComment {
        id: "T1-me".into(),
        author: "me".into(),
        created: "2026-09-08T09:00:00Z".into(),
        body: "mine".into(),
        reply_to: Some("T1-root".into()),
        finding: None,
    });
    let mut m1 = thread("M1", "src/lib.rs", "new", Some(8));
    m1.comments[0].author = "me".into();
    s.set_threads(vec![t1, m1]).unwrap();

    // Not told who I am: only a linked record makes a comment mine.
    assert!(s.own_comment("T1", "T1-me").is_none());
    assert!(s.own_root("M1").is_none());

    s.set_me("me".into());
    assert!(
        s.own_comment("T1", "T1-root").is_none(),
        "alice's root is not mine"
    );
    let reply = s
        .own_comment("T1", "T1-me")
        .expect("my reply is mine by author");
    assert_eq!(
        (reply.finding, reply.body.as_str(), reply.at.as_str()),
        (None, "mine", "src/lib.rs:3")
    );
    assert!(s.own_root("M1").is_some());
    assert!(s.own_root("T1").is_none());

    // Linked by a publish's address, twin not fetched: still mine, with the record.
    let id = s.add_finding(h3, None, "note".into()).unwrap().id.clone();
    s.mark_published(&[forge::Published {
        finding: id.clone(),
        thread: "T9".into(),
        comment: "C9".into(),
        url: None,
    }])
    .unwrap();
    let own = s.own_of_finding(&id).expect("published, so on the forge");
    assert_eq!(
        (
            own.thread.as_str(),
            own.comment.as_str(),
            own.finding.as_deref()
        ),
        ("T9", "C9", Some(id.as_str()))
    );
    assert!(s.own_of_finding("nope").is_none());
}

#[test]
fn a_linked_record_follows_an_edit_or_delete_even_before_its_twin_is_fetched() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    let h3 = s.doc().hunks.iter().position(|h| h.new_start == 3).unwrap();
    let id = s
        .add_finding(h3, None, "first words".into())
        .unwrap()
        .id
        .clone();
    s.mark_published(&[forge::Published {
        finding: id.clone(),
        thread: "T".into(),
        comment: "C".into(),
        url: None,
    }])
    .unwrap();
    assert!(s.threads().is_empty(), "no twin fetched yet");
    assert!(s.edit_comment("T", "C", "second words".into()).unwrap());
    assert_eq!(s.findings()[0].body, "second words");
    assert!(s.delete_comment("T", "C").unwrap());
    assert!(s.findings().is_empty());
    assert!(!s.delete_comment("T", "C").unwrap(), "nothing left to know");
}

#[test]
fn gitlab_is_not_held_to_the_three_line_rule_and_an_unchanged_line_carries_both_numbers() {
    // Head inserts a line at the top and edits line 3 (now 4): every later
    // unchanged line is one higher on the new side than on the old.
    let r = TestRepo::new();
    let lines: Vec<String> = (1..=10).map(|i| format!("line_{i} = {i}")).collect();
    r.write("src/lib.rs", format!("{}\n", lines.join("\n")).as_bytes());
    let base = r.commit_all("base");
    let mut changed = lines.clone();
    changed[2] = "line_3 = 300".to_string();
    changed.insert(0, "inserted = 0".to_string());
    r.write("src/lib.rs", format!("{}\n", changed.join("\n")).as_bytes());
    let head = r.commit_all("head");
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    let (doc, _) = doc_and_view(&r, &base, &head);

    // A changed line has one number; an unchanged line far below has both.
    assert_eq!(forge::other_side_line(&doc, "src/lib.rs", "new", 1), None);
    assert_eq!(forge::other_side_line(&doc, "src/lib.rs", "new", 4), None);
    assert_eq!(
        forge::other_side_line(&doc, "src/lib.rs", "new", 9),
        Some(8)
    );
    assert_eq!(
        forge::other_side_line(&doc, "src/lib.rs", "old", 8),
        Some(9)
    );
    assert_eq!(forge::other_side_line(&doc, "src/lib.rs", "old", 3), None);

    let h = s.doc().hunks.iter().position(|h| h.new_start == 4).unwrap();
    s.add_finding(h, Some(lines_at("new", 9, 9)), "far below".into())
        .unwrap();
    let github = s.publish_plan(forge::ForgeKind::Github);
    assert!(
        github.batch.is_empty(),
        "GitHub: five lines from a change is out"
    );
    assert_eq!(github.excluded.len(), 1);
    let gitlab = s.publish_plan(forge::ForgeKind::Gitlab);
    assert_eq!(gitlab.batch.comments.len(), 1, "GitLab: any line goes");
    assert!(gitlab.excluded.is_empty());
    assert_eq!(gitlab.batch.comments[0].line, 9);
    assert_eq!(gitlab.batch.comments[0].other_line, Some(8));
}

fn lines_at(side: &str, start: u32, end: u32) -> Lines {
    lines(side, start, end)
}

#[test]
fn a_reply_on_a_thread_with_no_line_is_refused_not_lost() {
    let (r, base, head) = two_hunk_repo();
    let tmp = tempfile::TempDir::new().unwrap();
    let mut s = session(&r, &base, &head, tmp.path());
    let mut gone = thread("T9", "src/lib.rs", "new", None);
    gone.line_text = Some("nothing like this".into());
    s.set_threads(vec![gone]).unwrap();
    assert!(s.thread("T9").unwrap().anchor.is_none());
    let err = s.add_reply("T9", "into the void".into()).unwrap_err();
    assert!(err.to_string().contains("no line in this diff"), "{err}");
    assert!(
        s.findings().is_empty(),
        "nothing filed that nothing could reach"
    );
}

#[test]
fn a_request_whose_commits_are_only_on_the_remote_is_fetched_first() {
    // `origin` has base, then a head on a request ref; the clone has base only.
    let origin = TestRepo::new();
    origin.write("f.txt", b"one\n");
    let base = origin.commit_all("base");
    let clone = TestRepo::new();
    clone.git(&["remote", "add", "origin", origin.root.to_str().unwrap()]);
    clone.git(&["fetch", "-q", "origin", "main"]);
    clone.git(&["reset", "-q", "--hard", &base]);
    origin.write("f.txt", b"two\n");
    let head = origin.commit_all("head");
    origin.git(&["update-ref", "refs/pull/7/head", &head]);
    let repo = clone.repo();
    assert!(
        differential_engine::ports::Ancestry::commit_of(&repo, &head)
            .unwrap()
            .is_none(),
        "the clone does not have the head yet"
    );

    let req = Request {
        kind: forge::ForgeKind::Github,
        project: "owner/repo".into(),
        id: "7".into(),
        base_ref: "main".into(),
        base_tip: base.clone(),
        head: head.clone(),
        merge_base: None,
        url: "https://example.invalid/pull/7".into(),
    };
    let source = forge::source_for(&repo, &req).unwrap();
    assert_eq!(source.head, head);
    assert_eq!(source.base, base);
    assert!(
        differential_engine::ports::Ancestry::commit_of(&repo, &head)
            .unwrap()
            .is_some(),
        "fetched"
    );

    // A commit no fetch can bring is still an error, with the hand-typed line.
    let gone = Request {
        head: "f".repeat(40),
        ..req
    };
    let err = forge::source_for(&repo, &gone).unwrap_err().to_string();
    assert!(err.contains("fetching did not bring them"), "{err}");
}
