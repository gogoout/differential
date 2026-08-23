//! End-to-end core pipeline: enumerate → annotate → classify → verify → emit.

use std::collections::HashSet;

use differential_schema as schema;

use crate::EngineError;
use crate::config::Config;
use crate::document::{SourceInfo, assemble, mark_generated};
use crate::gitio::Repo;
use crate::invariants::{InvariantReport, check_all};
use crate::lang::LanguageRegistry;
use crate::parse::parse_canonical;
use crate::rename_view::{merge_raw, merge_renames, parse_raw_z, parse_renames_z};
use crate::shape::partition;

pub struct PipelineOutput {
    pub base: String,
    pub head: String,
    pub report: InvariantReport,
    /// `None` iff an invariant failed — no document is emitted on a violation.
    pub document: Option<schema::PlanDocument>,
}

/// Resolve a revision-range spec into fully qualified endpoints.
/// Accepts `a..b`, `a...b` (base = merge-base, what an MR/PR diff is), or two
/// separate revs.
pub fn resolve_range(
    repo: &Repo,
    spec: &[&str],
) -> Result<(String, String, schema::SourceKind), EngineError> {
    match spec {
        [one] => {
            if let Some((a, b)) = one.split_once("...") {
                let base = repo.merge_base(a, b)?;
                Ok((base, b.to_string(), schema::SourceKind::Range))
            } else if let Some((a, b)) = one.split_once("..") {
                Ok((a.to_string(), b.to_string(), schema::SourceKind::Range))
            } else {
                Err(EngineError::Range(format!(
                    "single argument must be <base>..<head> or <a>...<b>, got {one:?}"
                )))
            }
        }
        [a, b] => Ok((
            (*a).to_string(),
            (*b).to_string(),
            schema::SourceKind::Range,
        )),
        other => Err(EngineError::Range(format!(
            "expected one range or two revs, got {} arguments",
            other.len()
        ))),
    }
}

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
    run_core(repo, base_rev, head_rev, kind, config, langs).map(|(out, _)| out)
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
    let (mut out, view) = run_core(repo, base_rev, head_rev, kind, config, langs)?;

    if let Some(core_doc) = &out.document {
        // Backend: injected, or built from [grouping] config.
        let built;
        let backend: &dyn differential_llm::LlmBackend = match grouping.backend {
            Some(b) => b,
            None => {
                built = backend_from_config(&config.grouping);
                &built
            }
        };
        let mut grouped = crate::grouping::run(
            core_doc,
            &view,
            backend,
            grouping.cache_dir,
            &langs.fingerprint(),
        )?;
        // Ordering is deterministic and model-free: always runs after grouping.
        crate::ordering::apply(&mut grouped, &view, langs);
        out.document = Some(grouped);
    }
    Ok(out)
}

pub struct StackOutput {
    pub pipeline: PipelineOutput,
    /// `None` iff invariants failed upstream (no document, nothing rendered).
    pub stack: Option<crate::stack::StackResult>,
}

/// Full production path for the shadow-branch renderer: core → group → order →
/// commit stack, sharing one internal diff view.
// Mirrors run_grouped_pipeline plus the stack options; bundling the two option
// structs further would be indirection for the lint's sake.
#[allow(clippy::too_many_arguments)]
pub fn run_stack_pipeline(
    repo: &Repo,
    base_rev: &str,
    head_rev: &str,
    kind: schema::SourceKind,
    config: &Config,
    langs: &LanguageRegistry,
    grouping: &crate::grouping::GroupingOptions,
    stack: &crate::stack::StackOptions,
) -> Result<StackOutput, EngineError> {
    let (mut out, view) = run_core(repo, base_rev, head_rev, kind, config, langs)?;

    let Some(core_doc) = &out.document else {
        return Ok(StackOutput {
            pipeline: out,
            stack: None,
        });
    };
    let built;
    let backend: &dyn differential_llm::LlmBackend = match grouping.backend {
        Some(b) => b,
        None => {
            built = backend_from_config(&config.grouping);
            &built
        }
    };
    let mut doc = crate::grouping::run(
        core_doc,
        &view,
        backend,
        grouping.cache_dir,
        &langs.fingerprint(),
    )?;
    crate::ordering::apply(&mut doc, &view, langs);
    let result = crate::stack::build_stack(repo, &doc, &view, stack)?;
    out.document = Some(doc);
    Ok(StackOutput {
        pipeline: out,
        stack: Some(result),
    })
}

fn backend_from_config(cfg: &crate::config::GroupingConfig) -> differential_llm::CommandBackend {
    let backend = match &cfg.command {
        Some(argv) if !argv.is_empty() => differential_llm::CommandBackend::new(
            argv.clone(),
            std::time::Duration::from_secs(1200),
        ),
        _ => differential_llm::CommandBackend::claude_cli(),
    };
    match cfg.timeout_secs {
        Some(s) => backend.with_timeout(std::time::Duration::from_secs(s)),
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
) -> Result<(PipelineOutput, crate::model::DiffView), EngineError> {
    let base = repo.rev_parse(base_rev)?;
    let head = repo.rev_parse(head_rev)?;

    // Canonical metadata: authoritative modes, full oids, dispositions.
    let raw = repo.run(
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
    let records = parse_raw_z(&raw)?;
    let dispositions = records
        .iter()
        .map(|r| (r.path.clone(), r.disposition()))
        .collect();

    // Canonical enumeration: every file, no exclusions (ADR 0005).
    let patch = repo.run(
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
    let mut view = parse_canonical(&patch, &dispositions)?;
    merge_raw(&mut view, &records)?;

    // Rename-detected annotations (ADR 0003).
    let renames_raw = repo.run(
        ["diff-tree", "-r", "-M", "-z", "--name-status", &base, &head],
        None,
    )?;
    merge_renames(&mut view, &parse_renames_z(&renames_raw)?);

    // Generated hints: gitattributes + config + builtins.
    let attr_marked = attr_marked_paths(repo, config, &view)?;
    mark_generated(&mut view, config, &attr_marked);

    // Mechanical partition: 100% coverage by construction.
    let part = partition(&view, langs);

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

    Ok((
        PipelineOutput {
            base,
            head,
            report,
            document,
        },
        view,
    ))
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
            if value != b"unspecified" && value != b"unset" && value != b"false" {
                marked.insert(path.to_vec());
            }
        }
    }
    Ok(marked)
}
