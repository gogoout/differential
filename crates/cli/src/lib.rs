//! The renderer binary (`dfr`, also installed as `differential`): render
//! surfaces over the engine's document, per ADR 0014. The engine stays the
//! single producer; this crate is argument parsing and presentation only.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::grouping::GroupingOptions;
use differential_engine::invariants::InvariantReport;
use differential_engine::lang::LanguageRegistry;
use differential_engine::pipeline::resolve_picked;
use differential_engine::plan;
use differential_engine::{resolve_range, run_pipeline};
use differential_stack::{StackOptions, run_stack_pipeline};

/// Grouped, ordered reading plans for large diffs.
#[derive(Parser)]
#[command(name = "dfr", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render the review commit stack onto a refs/review/… ref.
    Stack {
        #[command(flatten)]
        common: Common,
        /// Ref to land on (default: refs/review/<base7>-<head7>/stack).
        #[arg(long = "ref")]
        ref_name: Option<String>,
        /// Bypass the grouping cache (forces a fresh LLM call).
        #[arg(long)]
        no_cache: bool,
    },
    /// Run the pipeline and report the invariants (self-test / CI entry point).
    Check {
        #[command(flatten)]
        common: Common,
        /// Machine-readable report.
        #[arg(long)]
        json: bool,
    },
    /// Open the terminal reviewer over the grouped reading plan.
    Review {
        #[command(flatten)]
        common: Common,
        /// Bypass the grouping cache (forces a fresh LLM call).
        #[arg(long)]
        no_cache: bool,
    },
    /// Print the review's findings as JSON (re-anchored to the current plan).
    Findings {
        #[command(flatten)]
        common: Common,
        /// Bypass the grouping cache (forces a fresh LLM call).
        #[arg(long)]
        no_cache: bool,
    },
}

impl Command {
    fn common(&self) -> &Common {
        match self {
            Command::Stack { common, .. }
            | Command::Check { common, .. }
            | Command::Review { common, .. }
            | Command::Findings { common, .. } => common,
        }
    }
}

#[derive(Args)]
struct Common {
    /// Repository to operate on (defaults to the one containing the cwd).
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Repo config file (defaults to <repo-root>/.differential.toml).
    #[arg(long)]
    config: Option<PathBuf>,
    /// User config file (defaults to ~/.config/differential/config.toml).
    #[arg(long)]
    user_config: Option<PathBuf>,
    /// `<base>..<head>`, `<a>...<b>` (merge-base), or two revs. `review`
    /// without a range opens a picker (recent commits / staged / worktree).
    #[arg(num_args = 0..=2)]
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
    let common = cli.command.common();

    let dir = common
        .repo
        .clone()
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let repo = match Repo::open(&dir) {
        Ok(r) => r,
        Err(e) => return usage_error(&e.to_string()),
    };
    let config = match Config::load(
        repo.root(),
        common.config.as_deref(),
        common.user_config.as_deref(),
    ) {
        Ok(c) => c,
        Err(e) => return usage_error(&e.to_string()),
    };
    // Only `review` may omit the range (it opens the picker instead).
    let resolved = if common.range.is_empty() {
        if !matches!(cli.command, Command::Review { .. }) {
            return usage_error(
                "a revision range is required: <base>..<head>, <a>...<b>, or two revs",
            );
        }
        None
    } else {
        let spec: Vec<&str> = common.range.iter().map(String::as_str).collect();
        match resolve_range(&repo, &spec) {
            Ok(t) => Some(t),
            Err(e) => return usage_error(&e.to_string()),
        }
    };
    let langs = LanguageRegistry::builtin();

    match cli.command {
        Command::Stack {
            ref_name, no_cache, ..
        } => {
            let source = resolved.expect("range checked above");
            let cache_dir = cache_dir(&repo, no_cache)?;
            let out = run_stack_pipeline(
                &repo,
                &source,
                &config,
                &langs,
                &GroupingOptions {
                    backend: None, // from [grouping].command, default claude
                    cache_dir: cache_dir.as_deref(),
                    progress: None,
                    cancel: None,
                },
                &StackOptions {
                    ref_name: ref_name.as_deref(),
                },
            )
            .context("stack pipeline failed")?;

            let Some(stack) = out.stack else {
                eprintln!("error: invariants failed; nothing rendered");
                print_report(&out.pipeline.report, &out.pipeline.base, &out.pipeline.head);
                return Ok(ExitCode::from(1));
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
                    plan::short_oid(&c.sha),
                    c.hunks,
                    c.subject
                );
            }
            println!(
                "review with: git log --oneline {}..{}",
                plan::short_oid(&source.base),
                stack.ref_name
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Review { no_cache, .. } => {
            // The renderer owns the screen (picker -> splash -> reviewer); the
            // app layer owns what the pipeline is. Endpoints and review
            // identity per the wiring table in adr/0017.
            let pick = resolved.is_none();
            let cache_dir = cache_dir(&repo, no_cache)?;
            let worker_repo = repo.clone();
            differential_tui::review(&repo, pick, move |picked, tx, cancel| {
                // Which resolver runs is dispatch; what each one decides is
                // engine policy (ADR 0017).
                let source = match (resolved, picked) {
                    (Some(source), _) => source,
                    (None, Some(p)) => resolve_picked(&worker_repo, p.base, p.include_worktree)?,
                    (None, None) => anyhow::bail!("no review source picked"),
                };
                let report = move |p| {
                    let _ = tx.send(p);
                };
                let out = differential_engine::run_grouped_pipeline(
                    &worker_repo,
                    &source.base,
                    &source.head,
                    source.kind,
                    &config,
                    &langs,
                    &GroupingOptions {
                        backend: None,
                        cache_dir: cache_dir.as_deref(),
                        progress: Some(&report),
                        cancel: Some(cancel),
                    },
                )
                .context("grouped pipeline failed")?;
                let review_base = source.identity_base.unwrap_or_else(|| out.base.clone());
                Ok(differential_tui::Prepared {
                    out,
                    review_base,
                    head_spec: source.head_spec,
                })
            })?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Findings { no_cache, .. } => {
            let source = resolved.expect("range checked above");
            let out = grouped(&repo, &source, &config, &langs, no_cache)?;
            let doc = out
                .document
                .context("invariants failed; no plan available")?;
            let session = differential_engine::ReviewSession::open(
                &repo,
                &out.base,
                &source.head_spec,
                doc,
                out.view,
            )?;
            println!("{}", serde_json::to_string_pretty(session.findings())?);
            Ok(ExitCode::SUCCESS)
        }
        Command::Check { json, .. } => {
            let source = resolved.expect("range checked above");
            let out = run_pipeline(
                &repo,
                &source.base,
                &source.head,
                source.kind,
                &config,
                &langs,
            )
            .context("pipeline failed")?;
            if json {
                println!("{}", serde_json::to_string_pretty(&out.report)?);
            } else {
                print_report(&out.report, &out.base, &out.head);
            }
            Ok(if out.report.all_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
    }
}

fn usage_error(msg: &str) -> anyhow::Result<ExitCode> {
    eprintln!("error: {msg}");
    Ok(ExitCode::from(2))
}

fn print_report(report: &InvariantReport, base: &str, head: &str) {
    println!(
        "range      {}..{}",
        plan::short_oid(base),
        plan::short_oid(head)
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

/// The on-disk grouping cache (spec/persistence.md), unless bypassed.
fn cache_dir(repo: &Repo, no_cache: bool) -> anyhow::Result<Option<PathBuf>> {
    if no_cache {
        return Ok(None);
    }
    Ok(Some(plan::grouping_cache_dir(&repo.common_dir()?)))
}

/// Grouped pipeline with the on-disk cache (unless bypassed).
fn grouped(
    repo: &Repo,
    source: &differential_engine::plan::ReviewSource,
    config: &Config,
    langs: &LanguageRegistry,
    no_cache: bool,
) -> anyhow::Result<differential_engine::PipelineOutput> {
    let cache_dir = cache_dir(repo, no_cache)?;
    differential_engine::run_grouped_pipeline(
        repo,
        &source.base,
        &source.head,
        source.kind,
        config,
        langs,
        &GroupingOptions {
            backend: None,
            cache_dir: cache_dir.as_deref(),
            progress: None,
            cancel: None,
        },
    )
    .context("grouped pipeline failed")
}
