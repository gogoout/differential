//! Byte-level goldens for the things a refactor can break silently.
//!
//! The grouping cache is keyed by a content hash and stores the RAW model
//! response (ADR 0009). Three separate changes are individually invisible and
//! jointly catastrophic:
//!
//! - **The prompt text drifts while `PROMPT_VERSION` stays put.** Every cached
//!   entry then answers a prompt nobody would write today, and the cache still
//!   reports a hit.
//! - **The key composition changes.** Every user's cache misses at once, and
//!   the only symptom is that a re-run of a reviewed range calls the LLM again
//!   — which looks exactly like a slow first run.
//! - **The on-disk shape changes.** Existing entries become unreadable.
//!
//! None of these fail a behavioural test, so they are pinned here as exact
//! bytes. When one of these assertions fails, the question is never "what is
//! the new value" — it is whether `PROMPT_VERSION` should have been bumped.
//!
//! The sidecar layout is pinned for the same reason: it is the on-disk contract
//! that carries a reviewer's progress and findings across regenerations
//! (ADR 0013), and a moved file silently loses their work.

use std::collections::BTreeSet;

use differential_engine::config::Config;
use differential_engine::grouping::GroupingOptions;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_grouped_pipeline;
use differential_engine::schema::SourceKind;
use differential_engine::store::{FsArtefactStore, FsGroupingCache, FsReviewStore};
use differential_engine::{ReviewSession, review_state};
use differential_testutil::{FakeBackend, TestRepo, grouped_with_cache, json_group};

/// One 3-member class (an identifier swap repeated across three files) plus one
/// singleton behavioural class — enough to exercise class ordering, the
/// multi-file `(in: …)` annotation and both payload sides.
fn two_class_repo() -> (TestRepo, String, String) {
    let r = TestRepo::new();
    for name in ["a", "b", "c"] {
        r.write(
            &format!("src/{name}.txt"),
            b"use old_helper_name;\nother content\n",
        );
    }
    r.write("src/main.txt", b"fn main() { run_slowly() }\n");
    let base = r.commit_all("base");
    for name in ["a", "b", "c"] {
        r.write(
            &format!("src/{name}.txt"),
            b"use new_helper_name;\nother content\n",
        );
    }
    r.write("src/main.txt", b"fn main() { run_with_retries(3) }\n");
    let head = r.commit_all("head");
    (r, base, head)
}

fn one_group_backend(name: &str) -> FakeBackend {
    FakeBackend::new(name, |ids| {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Everything", "focus", &refs)
        )
    })
}

/// The exact bytes sent to the model, held as a **second, separate copy** of
/// the prompt.
///
/// It must never be the file `payload.rs` includes. The whole value of this
/// golden is that a prompt edit has to be made twice, deliberately; one shared
/// file would move both sides together and catch nothing.
///
/// The version in the name is the link to `PROMPT_VERSION`. A new prompt means
/// a new `prompt-v6.txt` and a new `include_str!` line — which is an act, not
/// an accident. Regenerating this file to match new output without bumping
/// `PROMPT_VERSION` serves cached groupings from the old prompt as if they were
/// current.
///
/// Two things in a real prompt cannot be frozen and are normalised away: the
/// artefact path, which is a temporary directory, and the base and head
/// revisions, which differ per range.
const EXPECTED_PROMPT: &str = include_str!("fixtures/prompt-v5.txt");

/// Largest class first, both diff sides, the multi-file annotation, and the
/// trailing blank line after every block.
#[test]
fn grouping_prompt_bytes_are_frozen() {
    let (r, base, head) = two_class_repo();
    let backend = one_group_backend("golden-backend");
    let _ = grouped_with_cache(&r, &base, &head, &backend, None);

    // The artefact path and the range are the parts of a real prompt that
    // cannot be frozen, so they are the parts normalised away.
    let prompt = regex::Regex::new(r"--doc \S+")
        .unwrap()
        .replace_all(&backend.last_prompt(), "--doc <DOC>")
        .into_owned();
    let prompt = regex::Regex::new(r"git diff [0-9a-f]{40} [0-9a-f]{40}")
        .unwrap()
        .replace_all(&prompt, "git diff <BASE> <HEAD>")
        .into_owned();
    assert_eq!(
        prompt, EXPECTED_PROMPT,
        "the grouping prompt changed; if this is intended, bump PROMPT_VERSION \
         so cached groupings from the old prompt are not served as current"
    );
}

/// The cache key hashes `PROMPT_VERSION`, the backend name, the language
/// fingerprint and the sorted member digests. Every one of those is pinned by
/// this single hex string.
#[test]
fn grouping_cache_key_and_entry_shape_are_frozen() {
    let (r, base, head) = two_class_repo();
    let cache = tempfile::TempDir::new().unwrap();
    let backend = one_group_backend("golden-backend");
    let _ = grouped_with_cache(&r, &base, &head, &backend, Some(cache.path()));

    let entries: Vec<_> = std::fs::read_dir(cache.path())
        .unwrap()
        .map(|e| e.unwrap())
        .collect();
    assert_eq!(entries.len(), 1, "one class partition, one cache entry");

    assert_eq!(
        entries[0].file_name().to_string_lossy(),
        "e597276e3f253ed8ac945ebca4bee25528b959df.json",
        "the grouping cache key changed; every existing cache entry in every \
         checkout just became unreachable, and the only symptom is a silent \
         re-run of the model"
    );

    // The stored value is the raw response, so audit and assembly stay pure
    // functions replayed on load.
    let body = std::fs::read_to_string(entries[0].path()).unwrap();
    assert_eq!(
        body,
        r#"{"response":"{\"groups\": [{\"label\": \"Everything\", \"description\": \"d\", \"classes\": [\"C0\", \"C1\"], \"effort\": \"focus\", \"reason\": \"r\"}]}"}"#,
        "the cache entry shape changed; existing entries no longer parse"
    );
}

/// A second run over the same partition must hit the cache and leave the model
/// alone. This is the property the key exists for, and nothing else asserts it
/// end to end.
#[test]
fn a_second_run_hits_the_cache_and_does_not_call_the_model() {
    let (r, base, head) = two_class_repo();
    let cache = tempfile::TempDir::new().unwrap();
    let backend = one_group_backend("golden-backend");

    let first = grouped_with_cache(&r, &base, &head, &backend, Some(cache.path()));
    assert_eq!(backend.calls(), 1);

    let second = grouped_with_cache(&r, &base, &head, &backend, Some(cache.path()));
    assert_eq!(backend.calls(), 1, "the second run must not call the model");

    assert_eq!(
        first.to_json().unwrap(),
        second.to_json().unwrap(),
        "a cached grouping must reproduce the document byte for byte"
    );
}

/// The sidecar layout under the review directory (ADR 0013). A moved or
/// renamed file here silently discards a reviewer's progress and findings on
/// upgrade, which no behavioural test would notice.
#[test]
fn review_sidecar_layout_is_frozen() {
    let (r, base, head) = two_class_repo();
    let backend = one_group_backend("fake");
    let out = run_grouped_pipeline(
        &r.repo(),
        &base,
        &head,
        SourceKind::Range,
        &Config::default(),
        &LanguageRegistry::builtin(),
        &GroupingOptions {
            backend: &backend,
            cache: &FsGroupingCache::disabled(),
            artefacts: &FsArtefactStore::disabled(),
            fetch: "dfr",
            progress: None,
        },
    )
    .unwrap();

    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("review");
    let store = FsReviewStore::at(root.clone()).unwrap();
    let mut session = ReviewSession::open(store, out.document.unwrap(), out.view).unwrap();
    let plan_hash = session.plan_hash().to_string();
    session.toggle_reviewed(0).unwrap();
    session.add_finding(0, None, "a finding".into()).unwrap();

    let names: BTreeSet<String> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["current", "findings.jsonl", "plans", "state.json"]
            .map(String::from)
            .into_iter()
            .collect::<BTreeSet<_>>()
    );

    // `current` points at a content-addressed plan, so findings re-anchor
    // against the exact document they were written on.
    assert_eq!(
        std::fs::read_to_string(root.join("current")).unwrap(),
        plan_hash
    );
    assert!(
        root.join("plans")
            .join(format!("{plan_hash}.json"))
            .exists()
    );

    // One finding per line, so appending never rewrites what is already there.
    let findings = std::fs::read_to_string(root.join("findings.jsonl")).unwrap();
    assert_eq!(findings.lines().count(), 1);
    let parsed: review_state::Finding = serde_json::from_str(findings.lines().next().unwrap())
        .expect("a finding line must parse as a Finding");
    assert_eq!(parsed.body, "a finding");
    assert_eq!(parsed.plan_hash, plan_hash);
}
