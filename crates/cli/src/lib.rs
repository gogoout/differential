use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::pipeline::run_pipeline;
use differential_schema::SourceKind;

/// Grouped, ordered reading plans for large diffs.
#[derive(Parser)]
#[command(name = "dfr", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Emit the JSON plan document for a diff.
    Plan {
        #[command(flatten)]
        common: Common,
        /// Pretty-print the JSON.
        #[arg(long)]
        pretty: bool,
        /// Write to a file instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Run the pipeline and report the invariants (self-test / CI entry point).
    Check {
        #[command(flatten)]
        common: Common,
        /// Machine-readable report.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args)]
struct Common {
    /// Repository to operate on (defaults to the one containing the cwd).
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Config file (defaults to <repo-root>/.differential.toml).
    #[arg(long)]
    config: Option<PathBuf>,
    /// `<base>..<head>`, `<a>...<b>` (merge-base), or two revs.
    #[arg(required = true, num_args = 1..=2)]
    range: Vec<String>,
}

/// Shared entry point for the `differential` and `dfr` binaries.
pub fn main_impl() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let (common, mode) = match &cli.command {
        Command::Plan {
            common,
            pretty,
            output,
        } => (
            common,
            Mode::Plan {
                pretty: *pretty,
                output: output.clone(),
            },
        ),
        Command::Check { common, json } => (common, Mode::Check { json: *json }),
    };

    let dir = common
        .repo
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let repo = match Repo::open(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(ExitCode::from(2));
        }
    };

    let config = match Config::load(repo.root(), common.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(ExitCode::from(2));
        }
    };

    let (base, head, kind) = match resolve_range(&repo, &common.range) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e:#}");
            return Ok(ExitCode::from(2));
        }
    };

    let out = run_pipeline(&repo, &base, &head, kind, &config).context("pipeline failed")?;

    match mode {
        Mode::Plan { pretty, output } => match out.document {
            Some(doc) => {
                let json = if pretty {
                    doc.to_json_pretty()?
                } else {
                    doc.to_json()?
                };
                match output {
                    Some(path) => std::fs::write(&path, json + "\n")
                        .with_context(|| format!("writing {}", path.display()))?,
                    None => println!("{json}"),
                }
                Ok(ExitCode::SUCCESS)
            }
            None => {
                eprintln!("error: invariants failed; no document emitted");
                print_report(&out.report, &out.base, &out.head, true);
                Ok(ExitCode::from(1))
            }
        },
        Mode::Check { json } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&out.report)?);
            } else {
                print_report(&out.report, &out.base, &out.head, false);
            }
            Ok(if out.report.all_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
    }
}

enum Mode {
    Plan {
        pretty: bool,
        output: Option<PathBuf>,
    },
    Check {
        json: bool,
    },
}

fn resolve_range(repo: &Repo, range: &[String]) -> anyhow::Result<(String, String, SourceKind)> {
    match range {
        [one] => {
            if let Some((a, b)) = one.split_once("...") {
                let base = repo.merge_base(a, b).context("resolving merge-base")?;
                Ok((base, b.to_string(), SourceKind::Range))
            } else if let Some((a, b)) = one.split_once("..") {
                Ok((a.to_string(), b.to_string(), SourceKind::Range))
            } else {
                anyhow::bail!("single argument must be <base>..<head> or <a>...<b>");
            }
        }
        [a, b] => Ok((a.clone(), b.clone(), SourceKind::Range)),
        _ => unreachable!("clap enforces 1..=2"),
    }
}

fn print_report(
    report: &differential_engine::invariants::InvariantReport,
    base: &str,
    head: &str,
    to_stderr: bool,
) {
    let mut lines = Vec::new();
    lines.push(format!(
        "range      {}..{}",
        &base[..12.min(base.len())],
        &head[..12.min(head.len())]
    ));
    lines.push(format!(
        "files      {} ({} binary, checked by oid only — tree assertion is tautological for those)",
        report.files_total, report.binary_oid_checked
    ));
    lines.push(format!("hunks      {}", report.hunks_total));
    lines.push(format!(
        "inv1 applier fidelity   {}  {}",
        report.applier_exact(),
        if report.applier_mismatches.is_empty() {
            "PASS"
        } else {
            "FAIL"
        }
    ));
    for m in &report.applier_mismatches {
        lines.push(format!("           mismatch: {m}"));
    }
    lines.push(format!(
        "inv2 hunk accounting    {}",
        if report.accounting_ok { "PASS" } else { "FAIL" }
    ));
    lines.push(format!(
        "inv3 tree assertion     {}  built {} head {}",
        if report.tree_ok { "PASS" } else { "FAIL" },
        report.built_tree.as_deref().unwrap_or("(not built)"),
        report.head_tree
    ));
    lines.push(format!(
        "inv4 independent recount {} of {}  {}",
        report.recount,
        report.hunks_total,
        if report.recount_ok { "PASS" } else { "FAIL" }
    ));
    lines.push(
        "note: tree building writes unreferenced loose objects into the odb (gc-able)".to_string(),
    );
    for l in lines {
        if to_stderr {
            eprintln!("{l}");
        } else {
            println!("{l}");
        }
    }
}
