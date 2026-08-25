//! Shadow-branch renderer tests: build the stack in real temp repos with a
//! fake LLM backend, then verify it with git itself.

use differential_engine::config::Config;
use differential_engine::grouping::GroupingOptions;
use differential_engine::lang::LanguageRegistry;
use differential_engine::llm::LlmBackend;
use differential_engine::plan::ReviewSource;
use differential_stack::run_stack_pipeline;
use differential_stack::{StackOptions, StackResult};
use differential_testutil::{FakeBackend, TestRepo, json_group};

fn stacked(r: &TestRepo, base: &str, head: &str, backend: &dyn LlmBackend) -> StackResult {
    let out = run_stack_pipeline(
        &r.repo(),
        &ReviewSource::range(base.into(), head.into(), head.into()),
        &Config::default(),
        &LanguageRegistry::builtin(),
        &GroupingOptions {
            backend: Some(backend),
            cache_dir: None,
            progress: None,
            cancel: None,
        },
        &StackOptions::default(),
    )
    .unwrap();
    out.stack.expect("stack built")
}

/// Two files with the same edit shape ×3 + one behavioural change.
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

fn focus_skim_backend() -> FakeBackend {
    FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}, {}]}}"#,
            json_group("Behaviour change", "focus", &[&ids[1]]),
            json_group("Helper rename", "skim", &[&ids[0]])
        )
    })
}

#[test]
fn stack_renders_focus_then_skim_split_and_lands_on_the_ref() {
    let (r, base, head) = two_class_repo();
    let s = stacked(&r, &base, &head, &focus_skim_backend());

    let subjects: Vec<&str> = s.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects.len(), 3);
    assert_eq!(subjects[0], "[focus] Behaviour change");
    assert!(subjects[1].starts_with("[skim 1/2] Helper rename — 1 exemplars"));
    assert!(subjects[2].starts_with("[skim 2/2] Helper rename — 2 further hunks"));
    assert_eq!(s.hunks_carried, 4);
    assert_eq!(s.recount, 4);

    // The ref exists and points at the tip; the tip tree IS the head tree.
    let tip = r.git(&["rev-parse", &s.ref_name]);
    assert_eq!(tip, s.tip);
    assert_eq!(
        r.git(&["rev-parse", &format!("{tip}^{{tree}}")]),
        r.git(&["rev-parse", &format!("{head}^{{tree}}")]),
    );

    // git log --oneline over the stack shows the reading plan, newest first.
    let log = r.git(&["log", "--format=%s", &format!("{base}..{tip}")]);
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[2].starts_with("[focus]"));
    assert!(lines[0].starts_with("[skim 2/2]"));

    // Trailer on every commit.
    let body = r.git(&["log", "--format=%b", &format!("{base}..{tip}")]);
    assert_eq!(body.matches("Review-Synthetic: ").count(), 3);
}

#[test]
fn skim_without_remainder_is_a_single_commit() {
    let r = TestRepo::new();
    r.write("one.txt", b"single_change_here = old\n");
    let base = r.commit_all("base");
    r.write("one.txt", b"single_change_here = new\n");
    let head = r.commit_all("head");
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Tiny", "skim", &[&ids[0]])
        )
    });
    let s = stacked(&r, &base, &head, &backend);
    assert_eq!(s.commits.len(), 1);
    assert!(
        s.commits[0]
            .subject
            .starts_with("[skim] Tiny — 1 exemplars")
    );
}

#[test]
fn zero_hunk_files_ride_the_meta_commit() {
    let r = TestRepo::new();
    r.write("run.sh", b"#!/bin/sh\necho hi\n");
    r.write("code.txt", b"real code\n");
    let base = r.commit_all("base");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            r.root.join("run.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    r.write("blob.bin", &[0u8, 1, 2, 0, 255]);
    r.write("empty.txt", b"");
    r.write("code.txt", b"real code changed\n");
    let head = r.commit_all("head");

    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Code", "focus", &[&ids[0]])
        )
    });
    let s = stacked(&r, &base, &head, &backend);
    let subjects: Vec<&str> = s.commits.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects.len(), 2);
    assert!(subjects[1].starts_with("[meta] 3 binary, mode or empty-file changes"));
    // Tree assertion held (build_stack errors otherwise) and the recount only
    // counts real hunks.
    assert_eq!(s.recount, 1);
}

#[test]
fn backfilled_group_renders_as_unclassified() {
    let (r, base, head) = two_class_repo();
    let backend = FakeBackend::new("fake", |ids| {
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Only one", "focus", &[&ids[1]])
        )
    });
    let s = stacked(&r, &base, &head, &backend);
    let last = &s.commits.last().unwrap().subject;
    assert_eq!(last, "[unclassified] 3 hunks carried by no group");
}

#[test]
fn deletions_and_noise_render_and_reconstruct() {
    let r = TestRepo::new();
    r.write("gone.txt", b"a\nb\nc\n");
    r.write("Cargo.lock", b"version = 1\n");
    r.write("kept.txt", b"same_line = old\n");
    let base = r.commit_all("base");
    std::fs::remove_file(r.root.join("gone.txt")).unwrap();
    r.write("Cargo.lock", b"version = 2\n");
    r.write("kept.txt", b"same_line = new\n");
    let head = r.commit_all("head");

    let backend = FakeBackend::new("fake", |ids| {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        format!(
            r#"{{"groups": [{}]}}"#,
            json_group("Everything", "focus", &refs)
        )
    });
    let s = stacked(&r, &base, &head, &backend);
    let subjects: Vec<&str> = s.commits.iter().map(|c| c.subject.as_str()).collect();
    assert!(subjects.iter().any(|s| s.starts_with("[noise]")));
    let tip_tree = r.git(&["rev-parse", &format!("{}^{{tree}}", s.tip)]);
    let head_tree = r.git(&["rev-parse", &format!("{head}^{{tree}}")]);
    assert_eq!(tip_tree, head_tree);
}

#[test]
fn submodule_bump_is_carried_as_a_gitlink() {
    let r = TestRepo::new();
    r.write("README.md", b"top\n");
    let sha_a = "a".repeat(40);
    let sha_b = "b".repeat(40);
    r.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{sha_a},vendor/dep"),
    ]);
    r.git(&["commit", "-q", "-m", "base"]);
    let base = r.git(&["rev-parse", "HEAD"]);
    r.git(&[
        "update-index",
        "--cacheinfo",
        &format!("160000,{sha_b},vendor/dep"),
    ]);
    r.write("README.md", b"top changed\n");
    r.git(&["add", "README.md"]);
    r.git(&["commit", "-q", "-m", "bump"]);
    let head = r.git(&["rev-parse", "HEAD"]);

    let backend = FakeBackend::new("fake", |ids| {
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        format!(r#"{{"groups": [{}]}}"#, json_group("All", "focus", &refs))
    });
    let s = stacked(&r, &base, &head, &backend);
    let tip_tree = r.git(&["rev-parse", &format!("{}^{{tree}}", s.tip)]);
    assert_eq!(tip_tree, r.git(&["rev-parse", &format!("{head}^{{tree}}")]));
    assert_eq!(s.recount, s.hunks_carried);
}

#[test]
fn custom_ref_name_is_honoured_and_rerun_is_idempotent() {
    let (r, base, head) = two_class_repo();
    let backend = focus_skim_backend();
    let out = run_stack_pipeline(
        &r.repo(),
        &ReviewSource::range(base.clone(), head.clone(), head.clone()),
        &Config::default(),
        &LanguageRegistry::builtin(),
        &GroupingOptions {
            backend: Some(&backend),
            cache_dir: None,
            progress: None,
            cancel: None,
        },
        &StackOptions {
            ref_name: Some("refs/review/custom/stack"),
        },
    )
    .unwrap();
    let s = out.stack.unwrap();
    assert_eq!(s.ref_name, "refs/review/custom/stack");
    assert_eq!(r.git(&["rev-parse", "refs/review/custom/stack"]), s.tip);

    // Re-running just moves the ref to a fresh, equally valid stack.
    let s2 = stacked(&r, &base, &head, &focus_skim_backend());
    assert_eq!(s2.hunks_carried, s.hunks_carried);
}
