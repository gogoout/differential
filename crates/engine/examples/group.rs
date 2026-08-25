//! Dev runner for the grouped pipeline. Prints a reading-plan summary to
//! stderr and the document JSON to stdout (or -o <file>).
//!
//! Usage: cargo run -p differential-engine --example group -- \
//!            [--repo <path>] [--config <path>] [--no-cache] [-o <file>] <base>..<head>
//!
//! Backend: [grouping].command from ~/.config/differential/config.toml, or the default
//! tools-denied claude invocation. Cache: <git-common-dir>/differential/cache/grouping.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::grouping::GroupingOptions;
use differential_engine::lang::LanguageRegistry;
use differential_engine::schema::Effort;
use differential_engine::store::{FsGroupingCache, OsConfigSource};
use differential_engine::{resolve_range, run_grouped_pipeline};

fn main() -> ExitCode {
    let mut repo_dir: Option<PathBuf> = None;
    let mut config_path: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut use_cache = true;
    let mut revs: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--repo" => repo_dir = args.next().map(PathBuf::from),
            "--config" => config_path = args.next().map(PathBuf::from),
            "-o" | "--output" => out_path = args.next().map(PathBuf::from),
            "--no-cache" => use_cache = false,
            "--help" | "-h" => {
                eprintln!(
                    "usage: group [--repo <path>] [--config <path>] [--no-cache] [-o <file>] <base>..<head>"
                );
                return ExitCode::from(2);
            }
            other => revs.push(other.to_string()),
        }
    }

    let dir = repo_dir.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let repo = match Repo::open(Path::new(&dir)) {
        Ok(r) => r,
        Err(e) => return usage_error(&e.to_string()),
    };
    let config = match Config::load(&OsConfigSource, repo.root(), config_path.as_deref(), None) {
        Ok(c) => c,
        Err(e) => return usage_error(&e.to_string()),
    };
    let spec: Vec<&str> = revs.iter().map(String::as_str).collect();
    let source = match resolve_range(&repo, &spec) {
        Ok(s) => s,
        Err(e) => return usage_error(&e.to_string()),
    };

    let cache = if use_cache {
        match FsGroupingCache::for_repo(&repo) {
            Ok(c) => c,
            Err(e) => return usage_error(&e.to_string()),
        }
    } else {
        FsGroupingCache::disabled()
    };
    // Composition is the application layer's job, so the example builds its
    // own backend rather than the pipeline reaching into config for one.
    let backend = differential_engine::llm::CommandBackend::claude_cli();

    let out = match run_grouped_pipeline(
        &repo,
        &source.base,
        &source.head,
        source.kind,
        &config,
        &LanguageRegistry::builtin(),
        &GroupingOptions {
            backend: &backend,
            cache: &cache,
            progress: None,
        },
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    let Some(doc) = out.document else {
        eprintln!("error: invariants failed; no document emitted");
        eprintln!("{:#?}", out.report);
        return ExitCode::from(1);
    };

    // Reading-plan summary, one line per group.
    if let Some(groups) = &doc.groups {
        let class_hunks = |g: &differential_engine::schema::Group| -> usize {
            g.class_ids
                .iter()
                .map(|cid| {
                    doc.classes
                        .iter()
                        .find(|c| &c.id == cid)
                        .map_or(0, |c| c.hunk_ids.len())
                })
                .sum()
        };
        for g in groups {
            let tier = match g.effort {
                Effort::Focus => "focus",
                Effort::Skim => "skim ",
                Effort::Noise => "noise",
            };
            eprintln!(
                "[{tier}] {:4}h /{:3} cls  {}",
                class_hunks(g),
                g.class_ids.len(),
                g.label
            );
        }
        eprintln!(
            "coverage {:?}  missing {:?}  read {:?}  skipped {:?}",
            doc.audit.coverage,
            doc.audit.classes_missing,
            doc.audit.read_hunks,
            doc.audit.skipped_hunks
        );
    }

    let json = doc.to_json_pretty().expect("serialise");
    match out_path {
        Some(p) => {
            if let Err(e) = std::fs::write(&p, json + "\n") {
                eprintln!("error writing {}: {e}", p.display());
                return ExitCode::from(1);
            }
        }
        None => println!("{json}"),
    }
    ExitCode::SUCCESS
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(2)
}
