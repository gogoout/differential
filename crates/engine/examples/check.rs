//! Dev/CI invariant runner. The engine is a library (ADR 0014); this example is
//! the runnable entry for regression sweeps, not a product CLI.
//!
//! Usage: cargo run -p differential-engine --example check -- \
//!            [--repo <path>] [--config <path>] <base>..<head> | <a>...<b> | <a> <b>

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::invariants::InvariantReport;
use differential_engine::lang::LanguageRegistry;
use differential_engine::{resolve_range, run_pipeline};

fn main() -> ExitCode {
    let mut repo_dir: Option<PathBuf> = None;
    let mut config_path: Option<PathBuf> = None;
    let mut revs: Vec<String> = Vec::new();

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--repo" => repo_dir = args.next().map(PathBuf::from),
            "--config" => config_path = args.next().map(PathBuf::from),
            "--help" | "-h" => {
                eprintln!("usage: check [--repo <path>] [--config <path>] <base>..<head>");
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

    match run_pipeline(
        &repo,
        &base,
        &head,
        kind,
        &config,
        &LanguageRegistry::builtin(),
    ) {
        Ok(out) => {
            print_report(&out.report, &out.base, &out.head);
            if out.report.all_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::from(2)
}

fn print_report(report: &InvariantReport, base: &str, head: &str) {
    println!(
        "range      {}..{}",
        &base[..12.min(base.len())],
        &head[..12.min(head.len())]
    );
    println!(
        "files      {} ({} binary, checked by oid only — tree assertion is tautological for those)",
        report.files_total, report.binary_oid_checked
    );
    println!("hunks      {}", report.hunks_total);
    println!(
        "inv1 applier fidelity   {}  {}",
        report.applier_exact(),
        if report.applier_mismatches.is_empty() {
            "PASS"
        } else {
            "FAIL"
        }
    );
    for m in &report.applier_mismatches {
        println!("           mismatch: {m}");
    }
    println!(
        "inv2 hunk accounting    {}",
        if report.accounting_ok { "PASS" } else { "FAIL" }
    );
    println!(
        "inv3 tree assertion     {}  built {} head {}",
        if report.tree_ok { "PASS" } else { "FAIL" },
        report.built_tree.as_deref().unwrap_or("(not built)"),
        report.head_tree
    );
    println!(
        "inv4 independent recount {} of {}  {}",
        report.recount,
        report.hunks_total,
        if report.recount_ok { "PASS" } else { "FAIL" }
    );
    println!("note: tree building writes unreferenced loose objects into the odb (gc-able)");
}
