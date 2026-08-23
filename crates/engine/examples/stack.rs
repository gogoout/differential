//! Dev runner for the shadow-branch renderer: builds the review commit stack
//! and prints its `git log --oneline`-shaped summary.
//!
//! Usage: cargo run -p differential-engine --example stack -- \
//!            [--repo <path>] [--config <path>] [--no-cache] [--ref <name>] <base>..<head>

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::grouping::GroupingOptions;
use differential_engine::lang::LanguageRegistry;
use differential_engine::stack::StackOptions;
use differential_engine::{resolve_range, run_stack_pipeline};

fn main() -> ExitCode {
    let mut repo_dir: Option<PathBuf> = None;
    let mut config_path: Option<PathBuf> = None;
    let mut ref_name: Option<String> = None;
    let mut use_cache = true;
    let mut revs: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--repo" => repo_dir = args.next().map(PathBuf::from),
            "--config" => config_path = args.next().map(PathBuf::from),
            "--ref" => ref_name = args.next(),
            "--no-cache" => use_cache = false,
            "--help" | "-h" => {
                eprintln!(
                    "usage: stack [--repo <path>] [--config <path>] [--no-cache] [--ref <name>] <base>..<head>"
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
    let config = match Config::load(repo.root(), config_path.as_deref()) {
        Ok(c) => c,
        Err(e) => return usage_error(&e.to_string()),
    };
    let spec: Vec<&str> = revs.iter().map(String::as_str).collect();
    let (base, head, kind) = match resolve_range(&repo, &spec) {
        Ok(t) => t,
        Err(e) => return usage_error(&e.to_string()),
    };
    let cache_dir = if use_cache {
        match repo.common_dir() {
            Ok(d) => Some(d.join("differential").join("cache").join("grouping")),
            Err(e) => return usage_error(&e.to_string()),
        }
    } else {
        None
    };

    let out = match run_stack_pipeline(
        &repo,
        &base,
        &head,
        kind,
        &config,
        &LanguageRegistry::builtin(),
        &GroupingOptions {
            backend: None,
            cache_dir: cache_dir.as_deref(),
        },
        &StackOptions {
            ref_name: ref_name.as_deref(),
        },
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };

    let Some(stack) = out.stack else {
        eprintln!("error: invariants failed; nothing rendered");
        eprintln!("{:#?}", out.pipeline.report);
        return ExitCode::from(1);
    };

    println!(
        "{}  ({} commits, {} hunks, recount {})",
        stack.ref_name,
        stack.commits.len(),
        stack.hunks_carried,
        stack.recount
    );
    for c in &stack.commits {
        println!(
            "  {}  {:4}h  {}",
            &c.sha[..10.min(c.sha.len())],
            c.hunks,
            c.subject
        );
    }
    println!(
        "review with: git log --oneline {}..{}",
        &base[..12.min(base.len())],
        stack.ref_name
    );
    ExitCode::SUCCESS
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(2)
}
