//! Shared test helper: hermetic temporary git repositories.
// Each test binary uses a different subset of these helpers.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::{PipelineOutput, run_pipeline};
use differential_schema::SourceKind;
use tempfile::TempDir;

pub struct TestRepo {
    pub _tmp: TempDir,
    pub root: PathBuf,
}

impl TestRepo {
    pub fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let r = TestRepo { _tmp: tmp, root };
        r.git(&["init", "-q", "-b", "main"]);
        r
    }

    pub fn git(&self, args: &[&str]) -> String {
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

    pub fn write(&self, path: &str, content: &[u8]) {
        let p = self.root.join(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, content).unwrap();
    }

    pub fn commit_all(&self, msg: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", msg]);
        self.git(&["rev-parse", "HEAD"])
    }

    pub fn repo(&self) -> Repo {
        Repo::open(Path::new(&self.root)).unwrap()
    }

    pub fn pipeline(&self, base: &str, head: &str) -> PipelineOutput {
        self.pipeline_with(base, head, &Config::default())
    }

    pub fn pipeline_with(&self, base: &str, head: &str, config: &Config) -> PipelineOutput {
        run_pipeline(
            &self.repo(),
            base,
            head,
            SourceKind::Range,
            config,
            &LanguageRegistry::builtin(),
        )
        .unwrap()
    }
}

pub fn assert_all_ok(out: &PipelineOutput) {
    assert!(out.report.all_ok(), "invariants failed: {:#?}", out.report);
    assert!(
        out.document.is_some(),
        "no document despite passing invariants"
    );
}

pub fn doc(out: &PipelineOutput) -> &differential_schema::PlanDocument {
    out.document.as_ref().unwrap()
}
