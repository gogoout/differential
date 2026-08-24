//! Real-corpus parity test. Committed code is fully generic: every repo path,
//! rev and expected number lives in an UNCOMMITTED local TOML (gitignored as
//! `*.local.toml`), pointed at by `DIFFERENTIAL_FIXTURE_CONFIG`. See
//! `fixtures.example.toml` at the workspace root for the shape.
//!
//! Run with: DIFFERENTIAL_FIXTURE_CONFIG=… cargo test -- --ignored

use std::path::Path;

use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::run_pipeline;
use differential_engine::schema::SourceKind;
use serde::Deserialize;

#[derive(Deserialize)]
struct FixtureFile {
    #[serde(default)]
    fixture: Vec<Fixture>,
}

#[derive(Deserialize)]
struct Fixture {
    repo_path: String,
    base: String,
    head: String,
    expect: Expect,
}

#[derive(Deserialize)]
struct Expect {
    files: u32,
    hunks: u32,
    classes: u32,
    applier: String,
    recount: u32,
}

#[test]
#[ignore = "needs DIFFERENTIAL_FIXTURE_CONFIG pointing at a local fixture file"]
fn real_corpus_parity() {
    let Ok(cfg_path) = std::env::var("DIFFERENTIAL_FIXTURE_CONFIG") else {
        eprintln!("skipping: DIFFERENTIAL_FIXTURE_CONFIG is not set");
        return;
    };
    let text = std::fs::read_to_string(&cfg_path)
        .unwrap_or_else(|e| panic!("cannot read {cfg_path}: {e}"));
    let fixtures: FixtureFile = toml::from_str(&text).expect("malformed fixture config");
    assert!(
        !fixtures.fixture.is_empty(),
        "fixture config contains no [[fixture]] entries"
    );

    for (i, fx) in fixtures.fixture.iter().enumerate() {
        let repo_path = shellexpand_home(&fx.repo_path);
        let repo = Repo::open(Path::new(&repo_path))
            .unwrap_or_else(|e| panic!("fixture {i}: cannot open {repo_path}: {e}"));
        let out = run_pipeline(
            &repo,
            &fx.base,
            &fx.head,
            SourceKind::Range,
            &Config::default(),
            &LanguageRegistry::builtin(),
        )
        .unwrap_or_else(|e| panic!("fixture {i}: pipeline failed: {e}"));

        assert!(
            out.report.all_ok(),
            "fixture {i}: invariants failed: {:#?}",
            out.report
        );
        let doc = out.document.expect("document");
        // Exact assertions: class-count drift is a normaliser-port bug, never
        // tolerance-adjusted away.
        assert_eq!(doc.stats.files, fx.expect.files, "fixture {i}: files");
        assert_eq!(doc.stats.hunks, fx.expect.hunks, "fixture {i}: hunks");
        assert_eq!(doc.stats.classes, fx.expect.classes, "fixture {i}: classes");
        assert_eq!(
            doc.audit.applier_exact, fx.expect.applier,
            "fixture {i}: applier"
        );
        assert_eq!(doc.audit.recount, fx.expect.recount, "fixture {i}: recount");
        assert_eq!(doc.audit.tree_assertion, "pass", "fixture {i}: tree");
        eprintln!(
            "fixture {i}: ok — {} files, {} hunks, {} classes",
            doc.stats.files, doc.stats.hunks, doc.stats.classes
        );
    }
}

fn shellexpand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{rest}");
    }
    p.to_string()
}
