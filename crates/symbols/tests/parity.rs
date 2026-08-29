//! Real-corpus measurement of the shipped readers.
//!
//! Committed code is fully generic: every repo path, rev and expected number
//! lives in an UNCOMMITTED local TOML (gitignored as `*.local.toml`), pointed
//! at by `DIFFERENTIAL_FIXTURE_CONFIG`. See `fixtures.example.toml` at the
//! workspace root for the shape.
//!
//! Run with: DIFFERENTIAL_FIXTURE_CONFIG=… cargo test -- --ignored
//!
//! **`sccs` is the number that matters, not `edges`.** A topological sort works
//! if and only if every strongly connected component has size one. One knot of
//! twelve classes made every class with an out-edge unorderable on the second
//! corpus range, and the ordering stage fell back to sorting by size — which is
//! the failure it exists to fix. Removing edges is only useful insofar as it
//! removes the false ones holding that knot together.

use std::collections::HashMap;
use std::path::Path;

use petgraph::algo::tarjan_scc;
use petgraph::graph::DiGraph;

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

/// Both optional: they pin **heuristics**, not facts. Files, hunks and classes
/// are what git and the normaliser must agree on, and the engine's own parity
/// test owns those. These two move whenever a reader changes, and they are
/// meant to — they are here so the movement is deliberate and visible.
#[derive(Deserialize)]
struct Expect {
    #[serde(default)]
    edges: Option<u32>,
    #[serde(default)]
    sccs: Option<u32>,
}

#[test]
#[ignore = "needs DIFFERENTIAL_FIXTURE_CONFIG pointing at a local fixture file"]
fn real_corpus_graph() {
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
            &differential_symbols::readers(),
        )
        .unwrap_or_else(|e| panic!("fixture {i}: pipeline failed: {e}"));

        assert!(
            out.report.all_ok(),
            "fixture {i}: invariants failed: {:#?}",
            out.report
        );
        let doc = out.document.expect("document");

        let index: HashMap<&str, usize> = doc
            .classes
            .iter()
            .enumerate()
            .map(|(j, c)| (c.id.as_str(), j))
            .collect();
        // `depends_on` is already one entry per target, so no edge repeats.
        let mut graph = DiGraph::<(), ()>::new();
        let nodes: Vec<_> = doc.classes.iter().map(|_| graph.add_node(())).collect();
        for (j, c) in doc.classes.iter().enumerate() {
            for e in &c.depends_on {
                if let Some(&target) = index.get(e.on.as_str()) {
                    graph.add_edge(nodes[j], nodes[target], ());
                }
            }
        }
        let edges = graph.edge_count();
        let knots: Vec<usize> = tarjan_scc(&graph)
            .into_iter()
            .map(|component| component.len())
            .filter(|&n| n > 1)
            .collect();
        let in_a_cycle: usize = knots.iter().sum();
        let biggest = knots.iter().copied().max().unwrap_or(0);

        eprintln!(
            "fixture {i}: {} classes, {edges} edges, {in_a_cycle} classes in {} cycles \
             (biggest {biggest})",
            doc.classes.len(),
            knots.len()
        );
        if let Some(expected) = fx.expect.edges {
            assert_eq!(
                edges as u32, expected,
                "fixture {i}: class dependency edges — a reader changed"
            );
        }
        if let Some(expected) = fx.expect.sccs {
            assert_eq!(
                in_a_cycle as u32, expected,
                "fixture {i}: classes inside a cycle — the ordering stage cannot \
                 order these, nor anything behind them"
            );
        }
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
