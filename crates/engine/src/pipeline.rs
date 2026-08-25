//! End-to-end core pipeline: enumerate → annotate → classify → verify → emit.

use std::collections::HashSet;

use crate::schema;

use crate::EngineError;
use crate::config::Config;
use crate::document::{SourceInfo, assemble};
use crate::gitio::Repo;
use crate::invariants::{InvariantReport, check_all};
use crate::lang::LanguageRegistry;
use crate::plan;

pub struct PipelineOutput {
    pub base: String,
    pub head: String,
    pub report: InvariantReport,
    /// `None` iff an invariant failed — no document is emitted on a violation.
    pub document: Option<schema::PlanDocument>,
    /// The canonical diff view (hunk content). Renderers that display bytes —
    /// the stack builder and the TUI — read from here; the schema document
    /// deliberately never carries content (ADR 0008).
    pub view: crate::model::DiffView,
}

/// Resolve a revision-range spec into a review source.
///
/// Accepts `a..b`, `a...b` (base = merge-base, what an MR/PR diff is), or two
/// separate revs. The spec is parsed once, in `plan::parse_range`, so the
/// endpoints and the review's identity cannot disagree about which side is the
/// head.
pub fn resolve_range(repo: &Repo, spec: &[&str]) -> Result<plan::ReviewSource, EngineError> {
    let parsed = plan::parse_range(spec)?;
    let head_spec = parsed.head_spec().to_string();
    let (base, head) = match &parsed {
        plan::RangeSpec::Direct { base, head } => (base.clone(), head.clone()),
        plan::RangeSpec::MergeBase { a, b } => (repo.merge_base(a, b)?, b.clone()),
    };
    Ok(plan::ReviewSource::range(base, head, head_spec))
}

/// Resolve the review picker's answer: a base commit, plus whether uncommitted
/// work is included (ADR 0017).
///
/// With it, the head is a snapshot of the worktree and the review is filed
/// under the base sha plus a stable literal, so it survives that snapshot tree
/// churning under it on every edit. Without it, the head is `HEAD`.
pub fn resolve_picked(
    repo: &Repo,
    base: String,
    include_worktree: bool,
) -> Result<plan::ReviewSource, EngineError> {
    if !include_worktree {
        return Ok(plan::ReviewSource::range(
            base,
            HEAD_SPEC.to_string(),
            HEAD_SPEC.to_string(),
        ));
    }
    let head = crate::worktree::worktree_tree(repo)?;
    Ok(plan::ReviewSource {
        identity_base: Some(base.clone()),
        base,
        head,
        kind: schema::SourceKind::Worktree,
        head_spec: WORKTREE_SPEC.to_string(),
    })
}

/// Identity literals for picked sources (ADR 0017). Not endpoints — they name
/// what the review is *of* while its synthesized endpoints move.
const HEAD_SPEC: &str = "HEAD";
const WORKTREE_SPEC: &str = "WORKTREE";

/// Run the core pipeline (stages: enumerate, classify) over `base..head`.
///
/// Config is consulted ONLY for classification hints; enumeration is total and
/// runs before config is even looked at (ADR 0012). Languages (ADR 0015) only
/// influence classification, never enumeration.
pub fn run_pipeline(
    repo: &Repo,
    base_rev: &str,
    head_rev: &str,
    kind: schema::SourceKind,
    config: &Config,
    langs: &LanguageRegistry,
) -> Result<PipelineOutput, EngineError> {
    run_core(repo, base_rev, head_rev, kind, config, langs)
}

/// Core pipeline + the grouping stage (stages: enumerate, classify, group).
///
/// The engine is the single producer of the final document renderers consume;
/// grouping runs in-process with internal access to the diff view. On any
/// invariant failure the grouping stage is skipped and `document` is `None`,
/// exactly like the core pipeline.
pub fn run_grouped_pipeline(
    repo: &Repo,
    base_rev: &str,
    head_rev: &str,
    kind: schema::SourceKind,
    config: &Config,
    langs: &LanguageRegistry,
    grouping: &crate::grouping::GroupingOptions,
) -> Result<PipelineOutput, EngineError> {
    let mut out = run_core_with_progress(
        repo,
        base_rev,
        head_rev,
        kind,
        config,
        langs,
        grouping.progress,
    )?;

    if let Some(core_doc) = &out.document {
        // Backend: injected, or built from [grouping] config.
        let built;
        let backend: &dyn crate::llm::LlmBackend = match grouping.backend {
            Some(b) => b,
            None => {
                built = backend_from_config(&config.grouping, grouping.cancel.clone());
                &built
            }
        };
        let mut grouped = crate::grouping::run(
            core_doc,
            &out.view,
            backend,
            grouping.cache_dir,
            &langs.fingerprint(),
            grouping.progress,
        )?;
        // Ordering is deterministic and model-free: always runs after grouping.
        if let Some(f) = grouping.progress {
            f(crate::grouping::Progress::Ordering);
        }
        crate::ordering::apply(&mut grouped, &out.view, langs);
        out.document = Some(grouped);
    }
    if let Some(f) = grouping.progress {
        f(crate::grouping::Progress::Done);
    }
    Ok(out)
}

fn backend_from_config(
    cfg: &crate::config::GroupingConfig,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> crate::llm::CommandBackend {
    let backend = match &cfg.command {
        Some(argv) if !argv.is_empty() => {
            crate::llm::CommandBackend::new(argv.clone(), std::time::Duration::from_secs(1200))
        }
        _ => crate::llm::CommandBackend::claude_cli(),
    };
    let backend = match cfg.timeout_secs {
        Some(s) => backend.with_timeout(std::time::Duration::from_secs(s)),
        None => backend,
    };
    match cancel {
        Some(flag) => backend.with_cancel(flag),
        None => backend,
    }
}

fn run_core(
    repo: &Repo,
    base_rev: &str,
    head_rev: &str,
    kind: schema::SourceKind,
    config: &Config,
    langs: &LanguageRegistry,
) -> Result<PipelineOutput, EngineError> {
    run_core_with_progress(repo, base_rev, head_rev, kind, config, langs, None)
}

fn run_core_with_progress(
    repo: &Repo,
    base_rev: &str,
    head_rev: &str,
    kind: schema::SourceKind,
    config: &Config,
    langs: &LanguageRegistry,
    progress: Option<&(dyn Fn(crate::grouping::Progress) + Send + Sync)>,
) -> Result<PipelineOutput, EngineError> {
    if let Some(f) = progress {
        f(crate::grouping::Progress::Enumerating);
    }
    // Commits normally; raw tree oids for uncommitted-state reviews
    // (ADR 0017) — every later stage treats the endpoints as trees anyway.
    let base = repo.rev_parse_commit_or_tree(base_rev)?;
    let head = repo.rev_parse_commit_or_tree(head_rev)?;

    // FROZEN ARGV. These three byte formats are what the parsers — and
    // ultimately the frozen normaliser — were validated against; a changed
    // flag changes shape hashes and breaks real-corpus parity (ADR 0002).
    let raw_records = repo.run(
        [
            "diff-tree",
            "-r",
            "-z",
            "--raw",
            "--full-index",
            "--no-renames",
            &base,
            &head,
        ],
        None,
    )?;
    let canonical_patch = repo.run(
        [
            "diff-tree",
            "-r",
            "-U0",
            "--no-renames",
            "--no-color",
            "--no-ext-diff",
            &base,
            &head,
        ],
        None,
    )?;
    let rename_records = repo.run(
        ["diff-tree", "-r", "-M", "-z", "--name-status", &base, &head],
        None,
    )?;

    // Enumeration is total and knows nothing about config (ADR 0012) — see
    // `plan::build_view`'s parameter list, which is where that is enforced.
    let mut view = plan::build_view(&plan::Enumeration {
        raw_records: &raw_records,
        canonical_patch: &canonical_patch,
        rename_records: &rename_records,
    })?;

    // Only now do config and languages get a say, and only over description.
    let attr_marked = attr_marked_paths(repo, config, &view)?;
    if let Some(f) = progress {
        f(crate::grouping::Progress::Classifying);
    }
    let part = plan::classify(&mut view, config, &attr_marked, langs);

    // Invariants 1–4; no document on violation.
    let report = check_all(repo, &base, &head, &view)?;
    let document = if report.all_ok() {
        Some(assemble(
            &view,
            &part,
            &SourceInfo {
                kind,
                base: base.clone(),
                head: head.clone(),
            },
            &report,
        )?)
    } else {
        None
    };

    Ok(PipelineOutput {
        base,
        head,
        report,
        document,
        view,
    })
}

/// Paths marked generated by any of the configured gitattributes names.
/// Note: `check-attr` consults the worktree/index `.gitattributes`, not the
/// reviewed revisions — acceptable for a hint that never affects enumeration.
fn attr_marked_paths(
    repo: &Repo,
    config: &Config,
    view: &crate::model::DiffView,
) -> Result<HashSet<Vec<u8>>, EngineError> {
    let mut marked = HashSet::new();
    if view.files.is_empty() {
        return Ok(marked);
    }
    let mut stdin: Vec<u8> = Vec::new();
    for f in &view.files {
        stdin.extend_from_slice(&f.path);
        stdin.push(0);
    }
    for attr in &config.attributes {
        let out = repo.run(["check-attr", "-z", "--stdin", attr.as_str()], Some(&stdin))?;
        // -z output: path NUL attr NUL value NUL ...
        let fields: Vec<&[u8]> = out.split(|&b| b == 0).collect();
        for triple in fields.chunks_exact(3) {
            let (path, value) = (triple[0], triple[2]);
            if plan::attr_marks_generated(value) {
                marked.insert(path.to_vec());
            }
        }
    }
    Ok(marked)
}
