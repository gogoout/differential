//! Review-state store tests: persistence, class content keys, and the
//! re-anchoring guarantees across a regenerated plan.

use differential_engine::config::Config;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_grouped_pipeline;
use differential_engine::ports::ReviewStore;
use differential_engine::review_state::{
    Anchor, Finding, FindingStatus, class_content_key, reanchor, review_id,
};
use differential_engine::schema::SourceKind;
use differential_engine::store::FsGroupingCache;
use differential_engine::store::FsReviewStore;
use differential_engine::{FsReviewSession, ReviewSession};
/// Findings hash their creation time into their id; pinning it keeps these
/// assertions about anchoring rather than about the clock.
const FIXED_TIME: u64 = 1_700_000_000;

use differential_testutil::{FakeBackend, TestRepo, grouped, json_group};

fn focus_all_backend() -> FakeBackend {
    FakeBackend::new("fake", |ids| {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Everything", "focus", &refs)
        )
    })
}

#[test]
fn review_id_is_stable_and_spec_sensitive() {
    assert_eq!(review_id("abc", "feature"), review_id("abc", "feature"));
    assert_ne!(review_id("abc", "feature"), review_id("abc", "other"));
    assert_ne!(review_id("abc", "feature"), review_id("def", "feature"));
}

#[test]
fn class_content_key_ignores_order() {
    let a = class_content_key(&["d2".into(), "d1".into()]);
    let b = class_content_key(&["d1".into(), "d2".into()]);
    assert_eq!(a, b);
    assert_ne!(a, class_content_key(&["d1".into()]));
}

#[test]
fn store_roundtrips_state_plans_and_findings() {
    let r = TestRepo::new();
    r.write("f.txt", b"alpha_value = 1\n");
    let base = r.commit_all("base");
    r.write("f.txt", b"alpha_value = 2\n");
    let head = r.commit_all("head");
    let doc = grouped(&r, &base, &head, &focus_all_backend());

    let tmp = tempfile::TempDir::new().unwrap();
    let store = FsReviewStore::at(tmp.path().join("rev1")).unwrap();

    let json = doc.to_json().unwrap();
    let hash = differential_engine::plan::plan_hash(&json);
    store.save_plan(&hash, &json).unwrap();
    // Idempotent: same doc, same hash, `current` points at it.
    // Content-addressed and idempotent: re-saving the same hash is a no-op.
    store.save_plan(&hash, &json).unwrap();

    let mut state = store.load_state().unwrap();
    assert!(state.reviewed_classes.is_empty());
    state.reviewed_classes.insert("k1".into());
    state.cursor = Some(("g0".into(), 4));
    store.save_state(&state).unwrap();
    let reloaded = store.load_state().unwrap();
    assert!(reloaded.reviewed_classes.contains("k1"));
    assert_eq!(reloaded.cursor, Some(("g0".into(), 4)));

    let f = Finding::new(
        FIXED_TIME,
        "off by one".into(),
        hash.clone(),
        Anchor {
            file: "f.txt".into(),
            side: "new".into(),
            line: 1,
            hunk_digest: doc.hunks[0].digest.clone(),
            line_text: "alpha_value = 2".into(),
            ..Anchor::default()
        },
    );
    store.save_findings(std::slice::from_ref(&f)).unwrap();
    let loaded = store.load_findings().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].body, "off by one");
    assert_eq!(loaded[0].status, FindingStatus::Open);
}

/// The core persistence promise: a finding anchored to a hunk survives new
/// The plan document AND the diff view for a range — `reanchor` needs both,
/// and `grouped` hands back only the document.
fn doc_and_view(
    r: &TestRepo,
    base: &str,
    head: &str,
) -> (
    differential_engine::schema::PlanDocument,
    differential_engine::model::DiffView,
) {
    let out = run_grouped_pipeline(
        &r.repo(),
        base,
        head,
        SourceKind::Range,
        &Config::default(),
        &LanguageRegistry::builtin(),
        &differential_engine::grouping::GroupingOptions {
            backend: &focus_all_backend(),
            cache: &FsGroupingCache::disabled(),
            progress: None,
        },
    )
    .unwrap();
    (out.document.unwrap(), out.view)
}

/// A finding anchors to an OFFSET inside its hunk, never to a line number.
/// The digest fixes the hunk's content, so a hunk that moved in the file still
/// holds the same line at the same offset — while the line number did not
/// survive the move.
#[test]
fn a_range_anchor_survives_the_hunk_moving_in_the_file() {
    let r = TestRepo::new();
    let block = |lead: usize, tail: &str| -> Vec<u8> {
        let mut out = String::new();
        for i in 1..=lead {
            out.push_str(&format!("pad{i} = {i}\n"));
        }
        out.push_str(tail);
        out.into_bytes()
    };
    let before = "first_changed = 1\nsecond_changed = 2\nthird_changed = 3\n";
    let after = "first_changed = 11\nsecond_changed = 22\nthird_changed = 33\n";

    r.write("f.txt", &block(3, before));
    let base = r.commit_all("base");
    r.write("f.txt", &block(3, after));
    let head1 = r.commit_all("head1");

    let (doc1, _) = doc_and_view(&r, &base, &head1);
    let hunk = &doc1.hunks[0];
    // Lines 4, 5, 6 of the new file; the hunk starts at 4, so offset 1 span 1.
    let start = hunk.new_start;
    let mut findings = vec![Finding::new(
        FIXED_TIME,
        "these two".into(),
        "plan1".into(),
        Anchor {
            file: "f.txt".into(),
            side: "new".into(),
            line: start + 1,
            end_line: start + 2,
            offset: 1,
            span: 1,
            hunk_digest: hunk.digest.clone(),
            line_text: "second_changed = 22".into(),
            end_line_text: "third_changed = 33".into(),
        },
    )];

    // Push the same change five lines down the file. The hunk's CONTENT is
    // untouched, so the digest matches and only its position changed.
    r.write("f.txt", &block(8, after));
    let head2 = r.commit_all("head2");
    let (doc2, view2) = doc_and_view(&r, &base, &head2);
    reanchor(&mut findings, &doc2, &view2, "plan2");

    // What the anchor has to survive is the LINES, whichever path re-anchored
    // it: the same two lines of the same file, five rows further down.
    let head_file = String::from_utf8(block(8, after)).unwrap();
    let line_of = |needle: &str| {
        head_file
            .lines()
            .position(|l| l == needle)
            .map(|i| i as u32 + 1)
            .unwrap_or_else(|| panic!("{needle:?} not in the head file"))
    };
    let a = &findings[0].anchor;
    assert_eq!(findings[0].status, FindingStatus::Open);
    assert_eq!(
        line_of("second_changed = 22"),
        start + 6,
        "the fixture should have pushed the block down five lines"
    );
    assert_eq!(
        a.line,
        line_of("second_changed = 22"),
        "the offset held; the line number it was written with did not"
    );
    assert_eq!(
        a.end_line,
        line_of("third_changed = 33"),
        "and so did the span"
    );
    let _ = doc2;
}

/// A record written before offsets existed has none. It has to land where it
/// always did: the hunk's first line.
#[test]
fn a_finding_without_an_offset_lands_on_the_hunks_first_line() {
    let r = TestRepo::new();
    r.write("f.txt", b"pad = 0\nalpha_value = 1\n");
    let base = r.commit_all("base");
    r.write("f.txt", b"pad = 0\nalpha_value = 2\n");
    let head = r.commit_all("head");

    let (doc, view) = doc_and_view(&r, &base, &head);

    // Exactly what serde gives an old record: everything additive defaulted.
    let json = format!(
        r#"{{"id":"old","created":1,"body":"b","status":"open","plan_hash":"old",
            "anchor":{{"file":"f.txt","side":"new","line":999,
                       "hunk_digest":"{}"}}}}"#,
        doc.hunks[0].digest
    );
    let mut findings: Vec<Finding> = vec![serde_json::from_str(&json).unwrap()];
    assert_eq!(findings[0].anchor.offset, 0);
    assert_eq!(findings[0].anchor.end_line, 0);
    assert_eq!(
        findings[0].anchor.line_span(),
        "999",
        "with no end_line it is one line, not a range to zero"
    );

    reanchor(&mut findings, &doc, &view, "new");
    let a = &findings[0].anchor;
    assert_eq!(a.line, doc.hunks[0].new_start);
    assert_eq!(
        a.end_line, a.line,
        "no span means the same line at both ends"
    );
    assert_eq!(a.line_span(), a.line.to_string());
}

/// commits on the branch — exact digest match when the hunk is untouched,
/// content match (flagged moved) when it shifted, orphaned when it vanished.
#[test]
fn findings_reanchor_across_regeneration() {
    let r = TestRepo::new();
    r.write("a.txt", b"stable_line = old_value\n");
    r.write("b.txt", b"other_content = old_thing\n");
    let base = r.commit_all("base");
    r.write("a.txt", b"stable_line = new_value\n");
    r.write("b.txt", b"other_content = new_thing\n");
    let head1 = r.commit_all("head1");

    let backend = focus_all_backend();
    let doc1 = grouped(&r, &base, &head1, &backend);
    let plan1 = "plan1hash";

    let digest_of = |doc: &differential_engine::schema::PlanDocument, file: &str| {
        doc.hunks
            .iter()
            .find(|h| h.file == file)
            .map(|h| h.digest.clone())
            .unwrap()
    };

    // Finding on a.txt's hunk, and one on b.txt's hunk.
    let mut findings = vec![
        Finding::new(
            FIXED_TIME,
            "check a".into(),
            plan1.to_string(),
            Anchor {
                file: "a.txt".into(),
                side: "new".into(),
                line: 1,
                hunk_digest: digest_of(&doc1, "a.txt"),
                line_text: "stable_line = new_value".into(),
                ..Anchor::default()
            },
        ),
        Finding::new(
            FIXED_TIME,
            "check b".into(),
            plan1.to_string(),
            Anchor {
                file: "b.txt".into(),
                side: "new".into(),
                line: 1,
                hunk_digest: digest_of(&doc1, "b.txt"),
                line_text: "other_content = new_thing".into(),
                ..Anchor::default()
            },
        ),
    ];

    // New commit: a.txt's change is untouched upstream but gains a line above
    // (same hunk content, shifted position → digest changes? No: digest is
    // content-exact and position-free, so it MATCHES). b.txt's change is
    // reworked entirely (old finding's hunk gone; line text gone) → orphan.
    r.write("a.txt", b"inserted_above = 1\nstable_line = new_value\n");
    r.write("b.txt", b"other_content = reworked_completely\n");
    let head2 = r.commit_all("head2");
    let out2 = run_grouped_pipeline(
        &r.repo(),
        &base,
        &head2,
        SourceKind::Range,
        &Config::default(),
        &LanguageRegistry::builtin(),
        &differential_engine::grouping::GroupingOptions {
            backend: &focus_all_backend(),
            cache: &FsGroupingCache::disabled(),
            progress: None,
        },
    )
    .unwrap();
    let doc2 = out2.document.unwrap();

    reanchor(&mut findings, &doc2, &out2.view, "plan2hash");

    // a.txt: the added line text still exists in a hunk → reattached (the
    // hunk content changed because the insertion merged into it under -U0,
    // so this lands on the content-match path, flagged moved).
    assert_eq!(findings[0].status, FindingStatus::Open);
    assert_eq!(findings[0].plan_hash, "plan2hash");
    assert!(
        doc2.hunks
            .iter()
            .any(|h| h.digest == findings[0].anchor.hunk_digest),
        "reattached digest must exist in the new plan"
    );

    // b.txt: gone entirely → orphaned, never dropped.
    assert_eq!(findings[1].status, FindingStatus::Orphaned);
    assert_eq!(findings.len(), 2);

    // A third regeneration that restores b's line revives the orphan.
    r.write("b.txt", b"other_content = new_thing\n");
    let head3 = r.commit_all("head3");
    let out3 = run_grouped_pipeline(
        &r.repo(),
        &base,
        &head3,
        SourceKind::Range,
        &Config::default(),
        &LanguageRegistry::builtin(),
        &differential_engine::grouping::GroupingOptions {
            backend: &focus_all_backend(),
            cache: &FsGroupingCache::disabled(),
            progress: None,
        },
    )
    .unwrap();
    reanchor(
        &mut findings,
        out3.document.as_ref().unwrap(),
        &out3.view,
        "plan3hash",
    );
    assert_eq!(findings[1].status, FindingStatus::Open);
    // Restored content is byte-identical to the original hunk, so revival
    // happens on the EXACT digest path — not even flagged as moved.
    assert!(!findings[1].moved);
}

/// The session owns persistence: every mutation is on disk before it returns,
/// verified by re-reading through an independent `ReviewStore`.
#[test]
fn session_persists_every_mutation() {
    let r = TestRepo::new();
    r.write("f.txt", b"alpha_value = 1\n");
    let base = r.commit_all("base");
    r.write("f.txt", b"alpha_value = 2\n");
    let head = r.commit_all("head");
    let out = run_grouped_pipeline(
        &r.repo(),
        &base,
        &head,
        SourceKind::Range,
        &Config::default(),
        &LanguageRegistry::builtin(),
        &differential_engine::grouping::GroupingOptions {
            backend: &focus_all_backend(),
            cache: &FsGroupingCache::disabled(),
            progress: None,
        },
    )
    .unwrap();
    let doc = out.document.unwrap();

    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("rev1");
    let mut session =
        ReviewSession::open(FsReviewStore::at(dir.clone()).unwrap(), doc, out.view).unwrap();
    let reread = || FsReviewStore::at(dir.clone()).unwrap();

    // The plan is persisted on open, `current` pointing at it.
    assert!(dir.join("current").exists());
    assert!(!session.plan_hash().is_empty());

    // toggle_reviewed: on, then off — each visible to a fresh store.
    assert!(session.toggle_reviewed(0).unwrap());
    assert_eq!(reread().load_state().unwrap().reviewed_classes.len(), 1);
    assert_eq!(session.reviewed_hunks(), std::iter::once(0).collect());
    assert!(!session.toggle_reviewed(0).unwrap());
    assert!(reread().load_state().unwrap().reviewed_classes.is_empty());

    // add_finding derives the anchor from the document + view.
    let id = {
        let f = session.add_finding(0, None, "off by one".into()).unwrap();
        assert_eq!(f.anchor.file, "f.txt");
        assert_eq!(f.anchor.side, "new");
        assert_eq!(f.anchor.line_text, "alpha_value = 2");
        f.id.clone()
    };
    assert_eq!(reread().load_findings().unwrap().len(), 1);

    // save_cursor round-trips.
    session.save_cursor("g0".into(), 7).unwrap();
    assert_eq!(
        reread().load_state().unwrap().cursor,
        Some(("g0".into(), 7))
    );
    assert_eq!(session.cursor(), Some(&("g0".to_string(), 7)));

    // delete_finding removes from disk; a bogus id is a no-op.
    assert!(session.delete_finding(&id).unwrap());
    assert!(!session.delete_finding("nope").unwrap());
    assert!(reread().load_findings().unwrap().is_empty());

    // set_reviewed is SET semantics over a batch, one write.
    let keys: Vec<String> = doc_class_keys(&session);
    session.set_reviewed(&keys, true).unwrap();
    assert_eq!(
        reread().load_state().unwrap().reviewed_classes.len(),
        keys.len()
    );
    // A partially reviewed set resolves to "all reviewed", never inverted.
    session.set_reviewed(&keys[..1], false).unwrap();
    session.set_reviewed(&keys, true).unwrap();
    assert_eq!(
        reread().load_state().unwrap().reviewed_classes.len(),
        keys.len()
    );
    session.set_reviewed(&keys, false).unwrap();
    assert!(reread().load_state().unwrap().reviewed_classes.is_empty());

    // set_split_diff / set_file_view round-trip (additive, default false).
    assert!(!session.split_diff());
    session.set_split_diff(true).unwrap();
    assert!(reread().load_state().unwrap().split_diff);
    assert!(!session.file_view());
    session.set_file_view(true).unwrap();
    assert!(reread().load_state().unwrap().file_view);
}

/// Class content keys of every class in the session's document.
fn doc_class_keys(session: &FsReviewSession) -> Vec<String> {
    session
        .doc()
        .classes
        .iter()
        .map(|c| session.class_key(&c.id).to_string())
        .collect()
}
