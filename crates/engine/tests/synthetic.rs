//! Synthetic-repo integration tests: every edge case runs the full pipeline
//! (enumerate → annotate → classify → invariants → document) against a real
//! temporary git repository built with plumbing-adjacent commands.

use std::path::{Path, PathBuf};
use std::process::Command;

use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::{PipelineOutput, run_pipeline};
use differential_schema::{Disposition, GeneratedBy, SourceKind};
use tempfile::TempDir;

struct TestRepo {
    _tmp: TempDir,
    root: PathBuf,
}

impl TestRepo {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let r = TestRepo { _tmp: tmp, root };
        r.git(&["init", "-q", "-b", "main"]);
        r
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-c")
            .arg("core.autocrlf=false")
            .arg("-c")
            .arg("user.name=test")
            .arg("-c")
            .arg("user.email=test@example.invalid")
            .args(args)
            .current_dir(&self.root)
            .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write(&self, path: &str, content: &[u8]) {
        let p = self.root.join(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    fn commit_all(&self, msg: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn pipeline(&self, base: &str, head: &str) -> PipelineOutput {
        self.pipeline_with(base, head, &Config::default())
    }

    fn pipeline_with(&self, base: &str, head: &str, config: &Config) -> PipelineOutput {
        let repo = Repo::open(Path::new(&self.root)).unwrap();
        run_pipeline(
            &repo,
            base,
            head,
            SourceKind::Range,
            config,
            &LanguageRegistry::builtin(),
        )
        .unwrap()
    }
}

fn assert_all_ok(out: &PipelineOutput) {
    assert!(out.report.all_ok(), "invariants failed: {:#?}", out.report);
    assert!(
        out.document.is_some(),
        "no document despite passing invariants"
    );
}

fn doc(out: &PipelineOutput) -> &differential_schema::PlanDocument {
    out.document.as_ref().unwrap()
}

// ---------------------------------------------------------------- newlines

#[test]
fn nonl_base_side_newline_added() {
    let r = TestRepo::new();
    r.write("f.txt", b"a\nlast");
    let base = r.commit_all("base");
    r.write("f.txt", b"a\nlast\n");
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    assert!(d.hunks[0].nonl_old);
    assert!(!d.hunks[0].nonl_new);
}

#[test]
fn nonl_head_side_newline_removed() {
    let r = TestRepo::new();
    r.write("f.txt", b"a\nlast\n");
    let base = r.commit_all("base");
    r.write("f.txt", b"a\nlast");
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    assert!(!d.hunks[0].nonl_old);
    assert!(d.hunks[0].nonl_new);
}

#[test]
fn nonl_both_sides() {
    let r = TestRepo::new();
    r.write("f.txt", b"old body");
    let base = r.commit_all("base");
    r.write("f.txt", b"new body");
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    assert!(d.hunks[0].nonl_old);
    assert!(d.hunks[0].nonl_new);
}

#[test]
fn nonl_created_file_without_final_newline() {
    let r = TestRepo::new();
    let base = r.commit_all("empty base");
    r.write("f.txt", b"no newline here");
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    assert!(doc(&out).hunks[0].nonl_new);
}

#[test]
fn nonl_deleted_file_without_final_newline() {
    let r = TestRepo::new();
    r.write("f.txt", b"no newline here");
    let base = r.commit_all("base");
    std::fs::remove_file(r.root.join("f.txt")).unwrap();
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    assert!(d.hunks[0].nonl_old);
    assert_eq!(d.files[0].disposition, Disposition::D);
}

// ---------------------------------------------------------------- zero-hunk files

#[test]
fn empty_file_add_and_delete() {
    let r = TestRepo::new();
    r.write("keep.txt", b"x\n");
    r.write("doomed.txt", b"");
    let base = r.commit_all("base");
    r.write("fresh.txt", b"");
    std::fs::remove_file(r.root.join("doomed.txt")).unwrap();
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    assert_eq!(d.stats.hunks, 0);
    assert_eq!(d.stats.files, 2);
    let fresh = d.files.iter().find(|f| f.path == "fresh.txt").unwrap();
    assert_eq!(fresh.disposition, Disposition::A);
    assert!(fresh.hunk_ids.is_empty());
}

#[test]
fn mode_only_change() {
    let r = TestRepo::new();
    r.write("run.sh", b"#!/bin/sh\necho hi\n");
    let base = r.commit_all("base");
    r.git(&["update-index", "--chmod=+x", "run.sh"]);
    r.git(&["commit", "-q", "-m", "chmod"]);
    let head = r.git(&["rev-parse", "HEAD"]);
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out); // fails under the prototype's hardcoded-100644 staging
    let d = doc(&out);
    assert_eq!(d.files[0].mode.as_deref(), Some("100755"));
    assert_eq!(d.files[0].old_mode.as_deref(), Some("100644"));
    assert!(d.files[0].hunk_ids.is_empty());
}

#[cfg(unix)]
#[test]
fn symlink_add_and_retarget() {
    let r = TestRepo::new();
    r.write("real.txt", b"content\n");
    let base = r.commit_all("base");
    std::os::unix::fs::symlink("real.txt", r.root.join("link")).unwrap();
    let mid = r.commit_all("add link");
    let out = r.pipeline(&base, &mid);
    assert_all_ok(&out);
    let d = doc(&out);
    let link = d.files.iter().find(|f| f.path == "link").unwrap();
    assert_eq!(link.mode.as_deref(), Some("120000"));

    std::fs::remove_file(r.root.join("link")).unwrap();
    r.write("other.txt", b"other\n");
    std::os::unix::fs::symlink("other.txt", r.root.join("link")).unwrap();
    let head = r.commit_all("retarget");
    let out = r.pipeline(&mid, &head);
    assert_all_ok(&out);
}

// ---------------------------------------------------------------- binary

#[test]
fn binary_add_and_modify() {
    let r = TestRepo::new();
    let base = r.commit_all("empty");
    r.write("blob.bin", &[0u8, 159, 146, 150, 0, 255, 1, 2]);
    let mid = r.commit_all("add binary");
    let out = r.pipeline(&base, &mid);
    assert_all_ok(&out);
    let d = doc(&out);
    assert!(d.files[0].binary);
    assert!(d.files[0].hunk_ids.is_empty());
    assert_eq!(d.stats.binary_files, 1);

    r.write("blob.bin", &[0u8, 1, 2, 3, 0, 4, 5]);
    let head = r.commit_all("modify binary");
    let out = r.pipeline(&mid, &head);
    assert_all_ok(&out);
    assert_eq!(doc(&out).stats.hunks, 0);
}

// ---------------------------------------------------------------- renames

#[test]
fn rename_verbatim_r100() {
    let r = TestRepo::new();
    let body = b"fn alpha() {}\nfn beta() {}\nfn gamma() {}\nfn delta() {}\nfn epsilon() {}\n";
    r.write("src/old_name.rs", body);
    let base = r.commit_all("base");
    r.git(&["mv", "src/old_name.rs", "src/new_name.rs"]);
    let head = r.commit_all("move");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    // Canonical view: D + A.
    assert_eq!(d.stats.files, 2);
    let added = d
        .files
        .iter()
        .find(|f| f.disposition == Disposition::A)
        .unwrap();
    let deleted = d
        .files
        .iter()
        .find(|f| f.disposition == Disposition::D)
        .unwrap();
    assert_eq!(added.rename_similarity, Some(100));
    assert_eq!(added.old_path.as_deref(), Some("src/old_name.rs"));
    assert_eq!(deleted.new_path.as_deref(), Some("src/new_name.rs"));
    assert_eq!(deleted.rename_similarity, Some(100));
}

#[test]
fn rename_with_heavy_edit_carries_low_similarity() {
    let r = TestRepo::new();
    let mut body = String::new();
    for i in 0..40 {
        body.push_str(&format!("shared_line_number_{i} = value_{i}\n"));
    }
    r.write("mod/original.txt", body.as_bytes());
    let base = r.commit_all("base");
    std::fs::remove_file(r.root.join("mod/original.txt")).unwrap();
    let mut edited = String::new();
    for i in 0..40 {
        if i % 4 == 0 {
            edited.push_str(&format!("rewritten_entry_{i} -> different({i})\n"));
        } else {
            edited.push_str(&format!("shared_line_number_{i} = value_{i}\n"));
        }
    }
    r.write("mod/relocated.txt", edited.as_bytes());
    let head = r.commit_all("move and rewrite");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    let added = d
        .files
        .iter()
        .find(|f| f.disposition == Disposition::A)
        .unwrap();
    let sim = added.rename_similarity.expect("rename must be detected");
    assert!(
        (50..95).contains(&sim),
        "similarity {sim} should be well below the relocation gate"
    );
    assert_eq!(added.old_path.as_deref(), Some("mod/original.txt"));
}

// ---------------------------------------------------------------- CRLF

#[test]
fn crlf_round_trips_and_shares_shape_with_lf_twin() {
    let r = TestRepo::new();
    r.write("dos.txt", b"alpha_value = one\r\nbeta_value = two\r\n");
    r.write("unix.txt", b"alpha_value = one\nbeta_value = two\n");
    let base = r.commit_all("base");
    r.write("dos.txt", b"alpha_value = CHANGED\r\nbeta_value = two\r\n");
    r.write("unix.txt", b"alpha_value = CHANGED\nbeta_value = two\n");
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out); // byte-faithful round trip is invariant 1
    let d = doc(&out);
    assert_eq!(d.stats.hunks, 2);
    // The CRLF and LF twins perform the same edit: one shape class.
    assert_eq!(d.stats.classes, 1, "normaliser must be CRLF-agnostic");
}

// ---------------------------------------------------------------- parser regressions

#[test]
fn deleted_lines_starting_with_dashes() {
    let r = TestRepo::new();
    r.write("cli.txt", b"--help\n--version\nplain\n");
    let base = r.commit_all("base");
    r.write("cli.txt", b"plain\n");
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    assert_eq!(doc(&out).stats.hunks, 1);
}

#[test]
fn insertion_at_top_and_deletion_to_empty() {
    let r = TestRepo::new();
    r.write("top.txt", b"body\n");
    r.write("empty_me.txt", b"a\nb\n");
    let base = r.commit_all("base");
    r.write("top.txt", b"header\nbody\n");
    r.write("empty_me.txt", b"");
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    let top_hunk = d.hunks.iter().find(|h| h.file == "top.txt").unwrap();
    assert_eq!((top_hunk.old_start, top_hunk.old_count), (0, 0));
    let emptied = d.files.iter().find(|f| f.path == "empty_me.txt").unwrap();
    assert_eq!(emptied.disposition, Disposition::M);
}

// ---------------------------------------------------------------- submodule

#[test]
fn submodule_bump_is_counted_and_carried() {
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
    r.git(&["commit", "-q", "-m", "bump"]);
    let head = r.git(&["rev-parse", "HEAD"]);

    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    assert_eq!(d.stats.hunks, 1, "pseudo-hunk stays in the canonical count");
    assert_eq!(d.stats.submodules, 1);
    let sub = d.files[0].submodule.as_ref().unwrap();
    assert_eq!(sub.old.as_deref(), Some(sha_a.as_str()));
    assert_eq!(sub.new.as_deref(), Some(sha_b.as_str()));
}

// ---------------------------------------------------------------- typechange

#[cfg(unix)]
#[test]
fn typechange_file_to_symlink() {
    let r = TestRepo::new();
    r.write("thing", b"real content\n");
    r.write("target.txt", b"t\n");
    let base = r.commit_all("base");
    std::fs::remove_file(r.root.join("thing")).unwrap();
    std::os::unix::fs::symlink("target.txt", r.root.join("thing")).unwrap();
    let head = r.commit_all("now a symlink");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    let f = d.files.iter().find(|f| f.path == "thing").unwrap();
    assert_eq!(f.mode.as_deref(), Some("120000"));
    assert_eq!(f.old_mode.as_deref(), Some("100644"));
}

// ---------------------------------------------------------------- paths

#[test]
fn path_with_spaces_survives() {
    let r = TestRepo::new();
    r.write("docs/design notes b.md", b"one\n");
    let base = r.commit_all("base");
    r.write("docs/design notes b.md", b"two\n");
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    assert_eq!(doc(&out).files[0].path, "docs/design notes b.md");
}

// ---------------------------------------------------------------- config

#[test]
fn config_glob_marks_generated_with_provenance() {
    let r = TestRepo::new();
    r.write("migrations/0001_init.sql", b"create table t (id int);\n");
    r.write("src/main.rs", b"fn main() {}\n");
    let base = r.commit_all("base");
    r.write("migrations/0001_init.sql", b"create table t (id bigint);\n");
    r.write("src/main.rs", b"fn main() { run() }\n");
    let head = r.commit_all("head");

    let cfg = Config::parse("[classify]\ngenerated = [\"migrations/**\"]", "test").unwrap();
    let out = r.pipeline_with(&base, &head, &cfg);
    assert_all_ok(&out);
    let d = doc(&out);
    let mig = d
        .files
        .iter()
        .find(|f| f.path.starts_with("migrations/"))
        .unwrap();
    assert!(mig.generated);
    assert_eq!(mig.generated_by, Some(GeneratedBy::Config));
    let main = d.files.iter().find(|f| f.path == "src/main.rs").unwrap();
    assert!(!main.generated);
}

#[test]
fn config_not_generated_overrides_builtin() {
    let r = TestRepo::new();
    r.write("important.lock", b"v1\n");
    let base = r.commit_all("base");
    r.write("important.lock", b"v2\n");
    let head = r.commit_all("head");

    let out = r.pipeline(&base, &head);
    assert_eq!(doc(&out).files[0].generated_by, Some(GeneratedBy::Builtin));

    let cfg = Config::parse("[classify]\nnot_generated = [\"important.lock\"]", "test").unwrap();
    let out = r.pipeline_with(&base, &head, &cfg);
    let f = &doc(&out).files[0];
    assert!(!f.generated);
    assert_eq!(f.generated_by, None);
}

#[test]
fn config_can_never_shrink_enumeration() {
    // ADR 0012 regression: a config marking half the tree generated must not
    // remove a single file or hunk from the canonical enumeration.
    let r = TestRepo::new();
    for i in 0..6 {
        r.write(
            &format!("gen/file_{i}.txt"),
            format!("old {i}\n").as_bytes(),
        );
        r.write(
            &format!("src/file_{i}.txt"),
            format!("old {i}\n").as_bytes(),
        );
    }
    let base = r.commit_all("base");
    for i in 0..6 {
        r.write(
            &format!("gen/file_{i}.txt"),
            format!("new {i}\n").as_bytes(),
        );
        r.write(
            &format!("src/file_{i}.txt"),
            format!("new {i}\n").as_bytes(),
        );
    }
    let head = r.commit_all("head");

    let plain = r.pipeline(&base, &head);
    let cfg = Config::parse("[classify]\ngenerated = [\"gen/**\"]", "test").unwrap();
    let hinted = r.pipeline_with(&base, &head, &cfg);
    assert_all_ok(&plain);
    assert_all_ok(&hinted);
    assert_eq!(doc(&plain).stats.files, doc(&hinted).stats.files);
    assert_eq!(doc(&plain).stats.hunks, doc(&hinted).stats.hunks);
    assert_eq!(doc(&hinted).stats.files, 12);
    assert_eq!(doc(&hinted).files.iter().filter(|f| f.generated).count(), 6);
}

#[test]
fn gitattributes_marks_generated_as_attr() {
    let r = TestRepo::new();
    r.write(".gitattributes", b"schema.out linguist-generated\n");
    r.write("schema.out", b"AUTOGENERATED v1\n");
    let base = r.commit_all("base");
    r.write("schema.out", b"AUTOGENERATED v2\n");
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let f = doc(&out)
        .files
        .iter()
        .find(|f| f.path == "schema.out")
        .unwrap();
    assert!(f.generated);
    assert_eq!(f.generated_by, Some(GeneratedBy::Attr));
}

// ---------------------------------------------------------------- document shape

#[test]
fn identical_edits_share_a_class_and_document_is_consistent() {
    let r = TestRepo::new();
    for name in ["a", "b", "c"] {
        r.write(
            &format!("src/{name}.txt"),
            b"use old_helper_name;\nother content\n",
        );
    }
    let base = r.commit_all("base");
    for name in ["a", "b", "c"] {
        r.write(
            &format!("src/{name}.txt"),
            b"use new_helper_name;\nother content\n",
        );
    }
    let head = r.commit_all("head");
    let out = r.pipeline(&base, &head);
    assert_all_ok(&out);
    let d = doc(&out);
    assert_eq!(d.stats.hunks, 3);
    assert_eq!(d.stats.classes, 1, "same edit in three files is one shape");
    let class = &d.classes[0];
    assert_eq!(class.hunk_ids.len(), 3);
    assert!(class.pure_substitution);
    // Round-trip through the frozen schema.
    let json = d.to_json_pretty().unwrap();
    let re = differential_schema::PlanDocument::from_json(&json).unwrap();
    assert_eq!(&re, d);
}
