//! Clearing the regenerable cache, and what it must not reach.
//!
//! Groupings and pre-group documents can be recomputed — at the cost of a model
//! call, which is why clearing them is a deliberate act. Findings cannot be
//! recomputed at all. The two live in sibling trees so that distinction is a
//! property of the layout rather than of this command being careful.

use std::path::Path;

use differential_engine::plan;
use differential_engine::ports::RepoLayout;
use differential_engine::store::{cache_usage, clear_cache};
use differential_testutil::TestRepo;

fn seed(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), body).unwrap();
}

/// A repo with two grouping responses, one pre-group document, and one review's
/// findings.
fn populated() -> (TestRepo, std::path::PathBuf) {
    let r = TestRepo::new();
    r.write("a.txt", b"a\n");
    r.commit_all("base");
    let repo = r.repo();
    let common = repo.common_dir().unwrap();
    seed(
        &plan::grouping_cache_dir(&common),
        "aaa.json",
        r#"{"response":"x"}"#,
    );
    seed(
        &plan::grouping_cache_dir(&common),
        "bbb.json",
        r#"{"response":"yy"}"#,
    );
    seed(&plan::artefact_dir(&common), "ccc.json", "{}");
    seed(
        &plan::review_dir(&common, "rev-1"),
        "state.json",
        "findings",
    );
    (r, common)
}

#[test]
fn clearing_the_cache_never_reaches_a_review() {
    let (r, common) = populated();
    let findings = plan::review_dir(&common, "rev-1").join("state.json");

    let removed = clear_cache(&r.repo()).unwrap();
    assert_eq!(removed.groupings, 2);
    assert_eq!(removed.documents, 1);

    assert!(!plan::cache_dir(&common).exists(), "the cache tree is gone");
    assert!(
        findings.exists(),
        "findings are not cache and must survive a clear"
    );
    assert_eq!(std::fs::read_to_string(&findings).unwrap(), "findings");
}

#[test]
fn measuring_does_not_remove_and_agrees_with_what_a_clear_reports() {
    let (r, common) = populated();
    let usage = cache_usage(&r.repo()).unwrap();
    assert_eq!((usage.groupings, usage.documents), (2, 1));
    // 16 + 17 bytes of grouping response, 2 of document.
    assert_eq!(usage.bytes, 35);
    assert!(plan::grouping_cache_dir(&common).join("aaa.json").exists());

    let removed = clear_cache(&r.repo()).unwrap();
    assert_eq!(
        (removed.groupings, removed.documents, removed.bytes),
        (usage.groupings, usage.documents, usage.bytes),
        "the CLI's --dry-run is this pair, so they must agree on one state"
    );
}

#[test]
fn clearing_an_already_clear_cache_is_not_an_error() {
    let r = TestRepo::new();
    r.write("a.txt", b"a\n");
    r.commit_all("base");
    let usage = clear_cache(&r.repo()).unwrap();
    assert!(usage.is_empty());
    // And again, now that the directory has certainly never existed.
    assert!(clear_cache(&r.repo()).unwrap().is_empty());
}

#[test]
fn the_cache_root_is_a_sibling_of_reviews_not_an_ancestor() {
    // The safety property, stated directly: no review path can ever sit under
    // the directory a clear removes.
    let common = Path::new("/tmp/repo/.git");
    assert!(!plan::review_dir(common, "rev-1").starts_with(plan::cache_dir(common)));
    assert!(plan::grouping_cache_dir(common).starts_with(plan::cache_dir(common)));
    assert!(plan::artefact_dir(common).starts_with(plan::cache_dir(common)));
}
