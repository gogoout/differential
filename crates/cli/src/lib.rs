//! The renderer binary (`dfr`, also installed as `differential`): render
//! surfaces over the engine's document, per ADR 0014. The engine stays the
//! single producer; this crate is argument parsing and presentation only.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use differential_engine::config::Config;
use differential_engine::gitio::Repo;
use differential_engine::grouping::GroupingOptions;
use differential_engine::lang::LanguageRegistry;
use differential_engine::llm::CommandBackend;
use differential_engine::pipeline::resolve_picked;
use differential_engine::plan;
use differential_engine::store::{FsGroupingCache, FsReviewStore, OsConfigSource};
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
        &OsConfigSource,
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
            let backend = backend_from(&config.grouping, None);
            let out = run_stack_pipeline(
                &repo,
                &source,
                &config,
                &langs,
                &GroupingOptions {
                    backend: &backend,
                    cache: &cache(&repo, no_cache)?,
                    progress: None,
                },
                &StackOptions {
                    ref_name: ref_name.as_deref(),
                },
            )
            .context("stack pipeline failed")?;

            let Some(stack) = out.stack else {
                eprintln!("error: invariants failed; nothing rendered");
                print_range(&out.pipeline.base, &out.pipeline.head);
                println!("{}", out.pipeline.report);
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
            let cache = cache(&repo, no_cache)?;
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
                        // The cancel flag lives on the backend: the thing that
                        // needs killing is the subprocess.
                        backend: &backend_from(&config.grouping, Some(cancel)),
                        cache: &cache,
                        progress: Some(&report),
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
            let store = FsReviewStore::for_review(&repo, &out.base, &source.head_spec)?;
            let session = differential_engine::ReviewSession::open(store, doc, out.view)?;
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
                print_range(&out.base, &out.head);
                println!("{}", out.report);
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

/// The reviewed range. Not part of `InvariantReport` — that struct is
/// serialised by `dfr check --json`, and growing it for a presentation
/// convenience is exactly the leak this refactor removes.
fn print_range(base: &str, head: &str) {
    println!(
        "range      {}..{}",
        plan::short_oid(base),
        plan::short_oid(head)
    );
}

/// The on-disk grouping cache, unless bypassed.
///
/// `--no-cache` is a state of the cache rather than an absent one, so the
/// grouping stage never grows a branch for it.
fn cache(repo: &Repo, no_cache: bool) -> anyhow::Result<FsGroupingCache> {
    Ok(if no_cache {
        FsGroupingCache::disabled()
    } else {
        FsGroupingCache::for_repo(repo)?
    })
}

/// Turn `[grouping]` config into a backend.
///
/// Composition, so it belongs to the application layer rather than the engine
/// (ADR 0018, 0020). Cancellation rides along here because killing an
/// in-flight subprocess is a property of the backend, not of the pipeline.
fn backend_from(
    cfg: &differential_engine::config::GroupingConfig,
    cancel: Option<Arc<AtomicBool>>,
) -> CommandBackend {
    let backend = match &cfg.command {
        Some(argv) if !argv.is_empty() => {
            CommandBackend::new(argv.clone(), Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        }
        _ => CommandBackend::claude_cli(),
    };
    let backend = match cfg.timeout_secs {
        Some(s) => backend.with_timeout(Duration::from_secs(s)),
        None => backend,
    };
    match cancel {
        Some(flag) => backend.with_cancel(flag),
        None => backend,
    }
}

/// Fallback when `[grouping].command` is set without a timeout.
const DEFAULT_TIMEOUT_SECS: u64 = 1200;

/// Grouped pipeline with the on-disk cache (unless bypassed).
fn grouped(
    repo: &Repo,
    source: &differential_engine::plan::ReviewSource,
    config: &Config,
    langs: &LanguageRegistry,
    no_cache: bool,
) -> anyhow::Result<differential_engine::PipelineOutput> {
    let backend = backend_from(&config.grouping, None);
    differential_engine::run_grouped_pipeline(
        repo,
        &source.base,
        &source.head,
        source.kind,
        config,
        langs,
        &GroupingOptions {
            backend: &backend,
            cache: &cache(repo, no_cache)?,
            progress: None,
        },
    )
    .context("grouped pipeline failed")
}
