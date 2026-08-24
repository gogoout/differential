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
    let common_range: Vec<String> = common.range.clone();
    // Only `review` may omit the range (it opens the picker instead).
    let resolved = if common_range.is_empty() {
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
            let (base, head, kind) = resolved.expect("range checked above");
            let cache_dir = if no_cache {
                None
            } else {
                Some(
                    repo.common_dir()?
                        .join("differential")
                        .join("cache")
                        .join("grouping"),
                )
            };
            let out = run_stack_pipeline(
                &repo,
                &base,
                &head,
                kind,
                &config,
                &langs,
                &GroupingOptions {
                    backend: None, // from [grouping].command, default claude
                    cache_dir: cache_dir.as_deref(),
                    progress: None,
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
            Ok(ExitCode::SUCCESS)
        }
        Command::Review { no_cache, .. } => {
            // Endpoints + review identity, either from the range or the
            // picker (see the wiring table in adr/0017 for the identity
            // rules on uncommitted sources).
            let (base, head, kind, head_spec, identity_base) = match resolved {
                Some((base, head, kind)) => (base, head, kind, head_spec_of(&common_range), None),
                None => match differential_tui::picker::pick_source(&repo)? {
                    None => return Ok(ExitCode::SUCCESS),
                    Some(differential_tui::picker::PickedSource::Commit { sha }) => (
                        sha,
                        "HEAD".to_string(),
                        differential_engine::schema::SourceKind::Range,
                        "HEAD".to_string(),
                        None,
                    ),
                    Some(differential_tui::picker::PickedSource::Staged) => {
                        let head_sha = repo.rev_parse("HEAD")?;
                        let index = differential_engine::worktree::index_tree(&repo)?;
                        (
                            head_sha.clone(),
                            index,
                            differential_engine::schema::SourceKind::Staged,
                            "INDEX".to_string(),
                            Some(head_sha),
                        )
                    }
                    Some(differential_tui::picker::PickedSource::Worktree) => {
                        let head_sha = repo.rev_parse("HEAD")?;
                        let index = differential_engine::worktree::index_tree(&repo)?;
                        let wt = differential_engine::worktree::worktree_tree(&repo)?;
                        (
                            index,
                            wt,
                            differential_engine::schema::SourceKind::Worktree,
                            "WORKTREE".to_string(),
                            Some(head_sha),
                        )
                    }
                },
            };
            let out = grouped(&repo, &base, &head, kind, &config, &langs, no_cache)?;
            // Identity: HEAD sha + stable literal for uncommitted sources
            // (the synthesized trees churn per edit); resolved base otherwise.
            let review_base = identity_base.unwrap_or_else(|| out.base.clone());
            differential_tui::run_review(&repo, out, &review_base, &head_spec)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Findings { no_cache, .. } => {
            let (base, head, kind) = resolved.expect("range checked above");
            let out = grouped(&repo, &base, &head, kind, &config, &langs, no_cache)?;
            let doc = out
                .document
                .context("invariants failed; no plan available")?;
            let session = differential_engine::ReviewSession::open(
                &repo,
                &out.base,
                &head_spec_of(&common_range),
                doc,
                out.view,
            )?;
            println!("{}", serde_json::to_string_pretty(session.findings())?);
            Ok(ExitCode::SUCCESS)
        }
        Command::Check { json, .. } => {
            let (base, head, kind) = resolved.expect("range checked above");
            let out = run_pipeline(&repo, &base, &head, kind, &config, &langs)
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

/// Grouped pipeline with the on-disk cache (unless bypassed).
fn grouped(
    repo: &Repo,
    base: &str,
    head: &str,
    kind: differential_engine::schema::SourceKind,
    config: &Config,
    langs: &LanguageRegistry,
    no_cache: bool,
) -> anyhow::Result<differential_engine::PipelineOutput> {
    let cache_dir = if no_cache {
        None
    } else {
        Some(
            repo.common_dir()?
                .join("differential")
                .join("cache")
                .join("grouping"),
        )
    };
    differential_engine::run_grouped_pipeline(
        repo,
        base,
        head,
        kind,
        config,
        langs,
        &GroupingOptions {
            backend: None,
            cache_dir: cache_dir.as_deref(),
            progress: None,
        },
    )
    .context("grouped pipeline failed")
}

/// The head endpoint AS TYPED — a branch name keeps the review's identity
/// stable while its tip moves.
fn head_spec_of(range: &[String]) -> String {
    match range {
        [one] => one
            .split_once("...")
            .or_else(|| one.split_once(".."))
            .map(|(_, b)| b.to_string())
            .unwrap_or_else(|| one.clone()),
        [_, b] => b.clone(),
        _ => String::new(),
    }
}
