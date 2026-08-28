//! Shared test support (publish = false): hermetic temporary git
//! repositories, the programmable fake LLM backend, and prompt helpers.
//! Dev-dependency of the engine and renderer crates — never published.

use std::path::{Path, PathBuf};
use std::process::Command;

use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::{PipelineOutput, run_pipeline};
use differential_engine::schema::SourceKind;
use tempfile::TempDir;

pub struct TestRepo {
    pub _tmp: TempDir,
    pub root: PathBuf,
}

impl Default for TestRepo {
    fn default() -> Self {
        Self::new()
    }
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

pub fn doc(out: &PipelineOutput) -> &differential_engine::schema::PlanDocument {
    out.document.as_ref().unwrap()
}

// ---------------------------------------------------------------- fake LLM

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use differential_engine::grouping::GroupingOptions;
use differential_engine::llm::{LlmBackend, LlmError};
use differential_engine::pipeline::run_grouped_pipeline;
use differential_engine::schema::PlanDocument;
use differential_engine::store::{FsArtefactStore, FsGroupingCache};

pub type Responder = Box<dyn Fn(&[String]) -> String + Send + Sync>;

/// Programmable backend: captures prompts, counts calls, and builds its
/// response from the class ids it actually sees (so tests never hardcode
/// partition-dependent ids).
pub struct FakeBackend {
    name: String,
    calls: AtomicUsize,
    prompts: Mutex<Vec<String>>,
    respond: Responder,
}

impl FakeBackend {
    pub fn new(name: &str, respond: impl Fn(&[String]) -> String + Send + Sync + 'static) -> Self {
        FakeBackend {
            name: name.to_string(),
            calls: AtomicUsize::new(0),
            prompts: Mutex::new(Vec::new()),
            respond: Box::new(respond),
        }
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn last_prompt(&self) -> String {
        self.prompts
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

impl LlmBackend for FakeBackend {
    fn name(&self) -> &str {
        &self.name
    }
    fn complete(&self, prompt: &str) -> Result<String, LlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(prompt.to_string());
        Ok((self.respond)(&ids_in_prompt(prompt)))
    }
}

/// The class ids the prompt offers, from its trailing id list.
///
/// The prompt no longer describes the classes at all — it names them and says
/// where to read about them (ADR 0022) — so this reads the last line, which is
/// the id list, in prompt order.
pub fn ids_in_prompt(prompt: &str) -> Vec<String> {
    prompt
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| {
            l.split_whitespace()
                .filter(|w| w.starts_with('C') && w[1..].chars().all(|c| c.is_ascii_digit()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn json_group(label: &str, effort: &str, classes: &[&str]) -> String {
    format!(
        r#"{{"label": "{label}", "description": "d", "classes": [{}], "effort": "{effort}", "reason": "r"}}"#,
        classes
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub fn grouped(r: &TestRepo, base: &str, head: &str, backend: &dyn LlmBackend) -> PlanDocument {
    grouped_with_cache(r, base, head, backend, None)
}

pub fn grouped_with_cache(
    r: &TestRepo,
    base: &str,
    head: &str,
    backend: &dyn LlmBackend,
    cache_dir: Option<&std::path::Path>,
) -> PlanDocument {
    let cache = match cache_dir {
        Some(dir) => FsGroupingCache::at(dir.to_path_buf()),
        None => FsGroupingCache::disabled(),
    };
    // Never inside `cache_dir`: the golden test counts what the grouping cache
    // wrote, and an artefact sitting next to it would be counted too.
    let artefacts = FsArtefactStore::disabled();
    let out = run_grouped_pipeline(
        &r.repo(),
        base,
        head,
        SourceKind::Range,
        &Config::default(),
        &LanguageRegistry::builtin(),
        &GroupingOptions {
            backend,
            cache: &cache,
            artefacts: &artefacts,
            fetch: "dfr",
            progress: None,
        },
    )
    .unwrap();
    out.document.expect("grouped document")
}
