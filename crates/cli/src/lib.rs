//! The renderer binary (`dfr`, also installed as `differential`): render
//! surfaces over the engine's document, per ADR 0014. The engine stays the
//! single producer; this crate is argument parsing and presentation only.

mod agent;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use differential_engine::config::{Agent, Config};
use differential_engine::gitio::Repo;
use differential_engine::grouping::GroupingOptions;
use differential_engine::lang::LanguageRegistry;
use differential_engine::llm::CommandBackend;
use differential_engine::pipeline::resolve_picked;
use differential_engine::plan;
use differential_engine::store::{
    self, FsArtefactStore, FsGroupingCache, FsReviewStore, OsConfigSource,
};
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
        /// Print the open findings as markdown instead — the same text the
        /// reviewer's `y` copies, for pasting into an agent or a PR.
        #[arg(long)]
        summary: bool,
        /// Bypass the grouping cache (forces a fresh LLM call).
        #[arg(long)]
        no_cache: bool,
    },
    /// Print every class the grouping model may group (ADR 0022).
    ///
    /// The grouping model's read path, and its whole read path: one call, one
    /// answer, no sub-questions. It takes no range, opens no repository and
    /// calls no model. Diff text is `git diff`'s job, which is why this needs
    /// no `--repo`.
    Agent {
        /// The document the grouping stage wrote.
        #[arg(long)]
        doc: PathBuf,
    },
    /// Delete the regenerable cache: grouping responses and pre-group
    /// documents.
    ///
    /// Findings are NOT cache and are never touched — reviews live in a sibling
    /// tree (ADR 0013). Cleaning costs a fresh model call on the next grouped
    /// run, so `--dry-run` reports what would go without removing it.
    ///
    /// Takes no range: the cache is the repository's, not a review's.
    Clean {
        /// Repository to operate on (defaults to the one containing the cwd).
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Report what would be removed, and remove nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

impl Command {
    /// `None` for the commands that take no range: `agent` works from a
    /// document, `clean` from the repository alone.
    fn common(&self) -> Option<&Common> {
        match self {
            Command::Stack { common, .. }
            | Command::Check { common, .. }
            | Command::Review { common, .. }
            | Command::Findings { common, .. } => Some(common),
            Command::Agent { .. } | Command::Clean { .. } => None,
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
    // `agent` opens no pipeline, no repository and no range, so it answers
    // before any of that is set up.
    if let Command::Agent { doc } = &cli.command {
        print!("{}", agent::run(doc)?);
        return Ok(ExitCode::SUCCESS);
    }

    // `clean` needs a repository but no range, no config and no languages, so
    // it answers before those are set up too.
    if let Command::Clean { repo, dry_run } = &cli.command {
        return clean(repo.as_deref(), *dry_run);
    }

    let common = cli.command.common().expect("agent and clean handled above");

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
    // The readers ARE the mechanism, not an optional set: a build that wires
    // none produces no dependency edges and so no foundation-first ordering.
    // Each reader ranks itself, so this call site cannot get the order wrong.
    let symbols = differential_symbols::readers();

    match cli.command {
        Command::Agent { .. } | Command::Clean { .. } => {
            unreachable!("handled before the pipeline is built")
        }
        Command::Stack {
            ref_name, no_cache, ..
        } => {
            let source = resolved.expect("range checked above");
            let backend = backend_from(&config.grouping, repo.root(), None);
            let out = run_stack_pipeline(
                &repo,
                &source,
                &config,
                &langs,
                &symbols,
                &GroupingOptions {
                    backend: &backend,
                    cache: &grouping_cache(&repo, no_cache)?,
                    artefacts: &artefact_store(&repo, no_cache)?,
                    fetch: &fetch_command(),
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
            let cache = grouping_cache(&repo, no_cache)?;
            let artefacts = artefact_store(&repo, no_cache)?;
            let fetch = fetch_command();
            let worker_repo = repo.clone();
            // Read before `config` moves into the pipeline closure: how much
            // context to show is presentation, so it goes to the renderer
            // rather than through the pipeline's result.
            let opts = differential_tui::ReviewOptions {
                context: config.review.context,
                context_step: config.review.context_step,
                split_diff: config.review.diff.is_split(),
                // As TYPED, so the footer can hand it straight back. Empty
                // when the picker chose the source, which has no spelling.
                range: (!common.range.is_empty()).then(|| common.range.join(" ")),
            };
            differential_tui::review(&repo, pick, opts, move |picked, tx, cancel| {
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
                    &symbols,
                    &GroupingOptions {
                        // The cancel flag lives on the backend: the thing that
                        // needs killing is the subprocess.
                        backend: &backend_from(&config.grouping, worker_repo.root(), Some(cancel)),
                        cache: &cache,
                        artefacts: &artefacts,
                        fetch: &fetch,
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
        Command::Findings {
            summary, no_cache, ..
        } => {
            let source = resolved.expect("range checked above");
            let out = grouped(&repo, &source, &config, &langs, &symbols, no_cache)?;
            let doc = out
                .document
                .context("invariants failed; no plan available")?;
            let store = FsReviewStore::for_review(&repo, &out.base, &source.head_spec)?;
            let session = differential_engine::ReviewSession::open(store, doc, out.view)?;
            // Two projections of one store, both the engine's: JSON for a
            // consumer, markdown for a person. The reviewer's `y` copies the
            // second one, so the two cannot drift.
            if summary {
                print!("{}", session.findings_summary());
            } else {
                println!("{}", serde_json::to_string_pretty(session.findings())?);
            }
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
                &symbols,
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
/// `dfr clean [--dry-run]`: report the regenerable cache, and unless asked not
/// to, delete it.
///
/// The count is taken before the delete either way, so the two modes report the
/// same thing about the same state.
fn clean(repo_dir: Option<&Path>, dry_run: bool) -> anyhow::Result<ExitCode> {
    let dir = repo_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));
    let repo = match Repo::open(&dir) {
        Ok(r) => r,
        Err(e) => return usage_error(&e.to_string()),
    };
    let usage = if dry_run {
        store::cache_usage(&repo)?
    } else {
        store::clear_cache(&repo)?
    };
    if usage.is_empty() {
        println!("cache is already empty");
        return Ok(ExitCode::SUCCESS);
    }
    let verb = if dry_run { "would remove" } else { "removed" };
    println!(
        "{verb} {} grouping {}, {} pre-group {} ({})",
        usage.groupings,
        plural(usage.groupings, "response", "responses"),
        usage.documents,
        plural(usage.documents, "document", "documents"),
        human_bytes(usage.bytes),
    );
    if !dry_run {
        println!("the next grouped run calls the model again; findings are untouched");
    }
    Ok(ExitCode::SUCCESS)
}

fn plural(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

/// Deliberately crude: this is one line of console output, and a crate for it
/// would be a dependency for three branches.
fn human_bytes(n: u64) -> String {
    if n < 1024 {
        return format!("{n} B");
    }
    // Round FIRST, then pick the unit. Choosing on the raw value instead prints
    // "1024.0 KiB" for the last byte below a mebibyte: 1_048_575 / 1024 is
    // 1023.999, which is under the threshold but rounds to 1024.0 on the way
    // out. The unit has to be chosen from what will actually be shown.
    let kib = (n as f64 / 1024.0 * 10.0).round() / 10.0;
    if kib < 1024.0 {
        format!("{kib:.1} KiB")
    } else {
        format!("{:.1} MiB", n as f64 / 1_048_576.0)
    }
}

fn grouping_cache(repo: &Repo, no_cache: bool) -> anyhow::Result<FsGroupingCache> {
    Ok(if no_cache {
        FsGroupingCache::disabled()
    } else {
        FsGroupingCache::for_repo(repo)?
    })
}

/// Where the model reads the pre-group document from.
///
/// `--no-cache` moves it to a temporary file rather than skipping it: the model
/// needs a path on every run, and only whether that path survives is what the
/// cache decides.
fn artefact_store(repo: &Repo, no_cache: bool) -> anyhow::Result<FsArtefactStore> {
    Ok(if no_cache {
        FsArtefactStore::disabled()
    } else {
        FsArtefactStore::for_repo(repo)?
    })
}

/// The executable the grouping model is told to fetch with (ADR 0022).
///
/// This process, so `cargo run`, an installed `dfr` and an installed
/// `differential` each name themselves. `dfr` is the fallback for the rare
/// platform where the current executable cannot be resolved; on that path the
/// model's fetches fail and it groups from the class ids alone.
///
/// The default backend's tool allowlist is derived from this same string, so
/// the prompt can never name a command the model is not allowed to run.
fn fetch_command() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "dfr".to_string())
}

/// Turn `[grouping]` config into a backend.
///
/// Composition, so it belongs to the application layer rather than the engine
/// (ADR 0018, 0020). Cancellation rides along here because killing an
/// in-flight subprocess is a property of the backend, not of the pipeline.
///
/// The `match` is what makes `Agent` worth being an enum: adding an agent adds
/// a variant there and an arm here, and the compiler names this line as the
/// second half of the job.
///
/// The agent runs **in the repository root**, because the prompt hands it
/// `git diff <base> <head> -- <path>` with paths as the document records them,
/// which is relative to that root. A child inheriting this process's cwd
/// matches nothing whenever `dfr` was run from a subdirectory, and matching
/// nothing is an empty diff and exit 0 rather than an error.
fn backend_from(
    cfg: &differential_engine::config::GroupingConfig,
    root: &std::path::Path,
    cancel: Option<Arc<AtomicBool>>,
) -> CommandBackend {
    let backend = match cfg.agent.unwrap_or_default() {
        Agent::ClaudeCode => CommandBackend::claude_cli(&fetch_command()),
    }
    .with_working_dir(root);
    let backend = match cfg.timeout_secs {
        Some(s) => backend.with_timeout(Duration::from_secs(s)),
        None => backend,
    };
    match cancel {
        Some(flag) => backend.with_cancel(flag),
        None => backend,
    }
}

/// Grouped pipeline with the on-disk cache (unless bypassed).
fn grouped(
    repo: &Repo,
    source: &differential_engine::plan::ReviewSource,
    config: &Config,
    langs: &LanguageRegistry,
    symbols: &differential_engine::artefact::symbols::SymbolReaders,
    no_cache: bool,
) -> anyhow::Result<differential_engine::PipelineOutput> {
    let backend = backend_from(&config.grouping, repo.root(), None);
    differential_engine::run_grouped_pipeline(
        repo,
        &source.base,
        &source.head,
        source.kind,
        config,
        langs,
        symbols,
        &GroupingOptions {
            backend: &backend,
            cache: &grouping_cache(repo, no_cache)?,
            artefacts: &artefact_store(repo, no_cache)?,
            fetch: &fetch_command(),
            progress: None,
        },
    )
    .context("grouped pipeline failed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_never_reads_as_a_full_unit_of_the_one_below() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        // The boundary the naive version got wrong.
        assert_eq!(human_bytes(1_048_575), "1.0 MiB");
        assert_eq!(human_bytes(1_048_576), "1.0 MiB");
        assert_eq!(human_bytes(1_572_864), "1.5 MiB");
    }
}
