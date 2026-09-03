//! The core pipeline: enumerate → annotate → classify → emit. **Read-only.**
//!
//! Its bound list carries no write port, and that is the point: the pipeline
//! cannot write, and a reader can see so without opening the body.
//!
//! Invariants 1 and 2 run here, and no document is emitted when either fails.
//! Invariant 1b runs earlier still, in `rename_view::merge_raw`. Those three
//! are what protect a renderer from a bad parse, a dropped file or broken
//! accounting.
//!
//! Invariants 3 and 4 build a tree, so they write. They live in [`verify`],
//! which a caller runs when it wants them — only a consumer that reconstructs
//! a tree is protected by them.

use std::collections::HashSet;

use crate::schema;

use crate::EngineError;
use crate::artefact::symbols::SymbolReaders;
use crate::config::Config;
use crate::document::{SourceInfo, apply_tree_audit, assemble};
use crate::invariants::{InvariantReport, check_fidelity, check_tree};
use crate::lang::LanguageRegistry;
use crate::plan;
use crate::ports::{
    AttributeSource, DiffSource, ObjectReader, ObjectWriter, RangeResolver, RecountSource,
    TreeBuilder, TreeResolver, WorkingCopy,
};
use crate::review_identity::WORKTREE_SPEC;

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
pub fn resolve_range<G: RangeResolver>(
    git: &G,
    spec: &[&str],
) -> Result<plan::ReviewSource, EngineError> {
    let parsed = plan::parse_range(spec)?;
    let head_spec = parsed.head_spec().to_string();
    let (base, head) = match &parsed {
        plan::RangeSpec::Direct { base, head } => (base.clone(), head.clone()),
        plan::RangeSpec::MergeBase { a, b } => (git.merge_base(a, b)?, b.clone()),
    };
    Ok(plan::ReviewSource::range(base, head, head_spec))
}

/// Resolve the review picker's answer: a base commit, plus whether uncommitted
/// work is included (ADR 0017).
///
/// With it, the head is a snapshot of the worktree and the review is filed
/// under the base sha plus a stable literal, so it survives that snapshot tree
/// churning under it on every edit. Without it, the head is `HEAD`.
pub fn resolve_picked<G>(
    git: &G,
    base: String,
    include_worktree: bool,
) -> Result<plan::ReviewSource, EngineError>
where
    G: TreeBuilder + WorkingCopy,
{
    if !include_worktree {
        return Ok(plan::ReviewSource::range(
            base,
            HEAD_SPEC.to_string(),
            HEAD_SPEC.to_string(),
        ));
    }
    let head = crate::worktree::worktree_tree(git)?;
    Ok(plan::ReviewSource {
        identity_base: Some(base.clone()),
        base,
        head,
        kind: schema::SourceKind::Worktree,
        head_spec: WORKTREE_SPEC.to_string(),
        remote: None,
    })
}

/// The identity literal for a picked `HEAD` source (ADR 0017). Not an endpoint
/// — it names what the review is *of* while its synthesized endpoints move.
///
/// Its worktree counterpart is `review_identity::WORKTREE_SPEC`, imported
/// rather than repeated: that module compares against the literal this one
/// writes, and two copies of it could drift into a review that is filed as
/// uncommitted work and then scanned as if it were a commit.
const HEAD_SPEC: &str = "HEAD";

/// Run the core pipeline (stages: enumerate, classify) over `base..head`.
///
/// Config is consulted ONLY for classification hints; enumeration is total and
/// runs before config is even looked at (ADR 0012). Languages (ADR 0015) only
/// influence classification, never enumeration.
pub fn run_pipeline<G>(
    git: &G,
    source: &plan::ReviewSource,
    config: &Config,
    langs: &LanguageRegistry,
    symbols: &SymbolReaders,
) -> Result<PipelineOutput, EngineError>
where
    G: RangeResolver + DiffSource + AttributeSource + ObjectReader,
{
    run_core_with_progress(git, source, config, langs, symbols, None)
}

/// Core pipeline + the grouping stage (stages: enumerate, classify, group).
///
/// The engine is the single producer of the final document renderers consume;
/// grouping runs in-process over the document the core stages produced — it
/// takes no diff view, because everything it needs is in the document and the
/// model fetches the rest (ADR 0022). On any invariant failure the grouping
/// stage is skipped and `document` is `None`, exactly like the core pipeline.
// The parameter list is the point, exactly as a bound list is: each entry is a
// distinct authority this function may use. Bundling `langs` and `symbols`
// behind a context struct would shorten the list without making it clearer,
// and `CLAUDE.md` rule 2 refuses that shape.
#[allow(clippy::too_many_arguments)]
pub fn run_grouped_pipeline<G, C, A>(
    git: &G,
    source: &plan::ReviewSource,
    config: &Config,
    langs: &LanguageRegistry,
    symbols: &SymbolReaders,
    grouping: &crate::grouping::GroupingOptions<C, A>,
) -> Result<PipelineOutput, EngineError>
where
    C: crate::ports::GroupingCache,
    A: crate::ports::ArtefactStore,
    G: RangeResolver + DiffSource + AttributeSource + ObjectReader,
{
    let mut out = run_core_with_progress(git, source, config, langs, symbols, grouping.progress)?;

    if let Some(core_doc) = &out.document {
        let mut grouped = crate::grouping::run(
            core_doc,
            grouping.backend,
            grouping.cache,
            grouping.artefacts,
            grouping.fetch,
            &langs.fingerprint(),
            &symbols.fingerprint(),
            grouping.progress,
        )?;
        // Ordering is deterministic and model-free: always runs after grouping.
        if let Some(f) = grouping.progress {
            f(crate::grouping::Progress::Ordering);
        }
        crate::ordering::apply(&mut grouped);
        out.document = Some(grouped);
    }
    if let Some(f) = grouping.progress {
        f(crate::grouping::Progress::Done);
    }
    Ok(out)
}

/// Invariants 3 and 4 over a pipeline's output. **This writes.**
///
/// Building a tree from the hunks is the only non-tautological way to prove
/// every hunk was carried, and `write-tree` needs the blobs in the odb. They
/// land unreferenced and `git gc` collects them.
///
/// Run it when the caller reconstructs a tree — `dfr check`, whose whole job
/// this is, and the shadow-branch builder, whose commits are trees built from
/// exactly these hunks. A reviewer that only reads a diff is protected by
/// invariants 1b, 1 and 2, which have already run.
///
/// The result lands in `out.report.tree` and in the document's audit block,
/// with `"verify"` appended to `generator.stages`. Absence of that stage is how
/// a consumer tells "did not run" from "ran and passed".
pub fn verify<G>(git: &G, out: &mut PipelineOutput) -> Result<(), EngineError>
where
    G: ObjectReader + ObjectWriter + TreeResolver + TreeBuilder + RecountSource,
{
    let tree = check_tree(git, &out.base, &out.head, &out.view, &out.report)?;
    if let Some(doc) = &mut out.document {
        apply_tree_audit(doc, &tree);
    }
    out.report.tree = Some(tree);
    Ok(())
}

// The parameter list is the point, exactly as a bound list is: each entry is a
// distinct authority this function may use. Bundling `langs` and `symbols`
// behind a context struct would shorten the list without making it clearer,
// and `CLAUDE.md` rule 2 refuses that shape.
#[allow(clippy::too_many_arguments)]
fn run_core_with_progress<G>(
    git: &G,
    source: &plan::ReviewSource,
    config: &Config,
    langs: &LanguageRegistry,
    symbols: &SymbolReaders,
    progress: Option<&(dyn Fn(crate::grouping::Progress) + Send + Sync)>,
) -> Result<PipelineOutput, EngineError>
where
    G: RangeResolver + DiffSource + AttributeSource + ObjectReader,
{
    if let Some(f) = progress {
        f(crate::grouping::Progress::Enumerating);
    }
    // Commits normally; raw tree oids for uncommitted-state reviews
    // (ADR 0017) — every later stage treats the endpoints as trees anyway.
    let base = git.resolve_endpoint(&source.base)?;
    let head = git.resolve_endpoint(&source.head)?;

    // The argv for these three is FROZEN and lives in the adapter, where a
    // reviewer can see all of it at once (ADR 0002).
    let raw_records = git.raw_records(&base, &head)?;
    let canonical_patch = git.canonical_patch(&base, &head)?;
    let rename_records = git.rename_records(&base, &head)?;

    // Enumeration is total and knows nothing about config (ADR 0012) — see
    // `plan::build_view`'s parameter list, which is where that is enforced.
    let mut view = plan::build_view(&plan::Enumeration {
        raw_records: &raw_records,
        canonical_patch: &canonical_patch,
        rename_records: &rename_records,
    })?;

    // Only now do config and languages get a say, and only over description.
    let attr_marked = attr_marked_paths(git, config, &view)?;
    if let Some(f) = progress {
        f(crate::grouping::Progress::Classifying);
    }
    let part = plan::classify(&mut view, config, &attr_marked, langs);
    // The dependency graph is classification too, and it is built from classes
    // rather than from groups: what depends on what is a fact about the diff,
    // so the model reads it before it groups and cannot change it by grouping
    // (ADR 0022).
    let graph = crate::artefact::graph::build(git, &head, &view, &part, symbols)?;

    // Invariants 1 and 2, read-only; no document on violation. The tree half
    // is `verify`'s, and a caller that never builds a tree never needs it.
    let report = check_fidelity(git, &base, &head, &view)?;
    let document = if report.fidelity_ok() {
        Some(assemble(
            &view,
            &part,
            graph,
            &SourceInfo {
                kind: source.kind,
                base: base.clone(),
                head: head.clone(),
                remote: source.remote.clone(),
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
fn attr_marked_paths<G: AttributeSource>(
    git: &G,
    config: &Config,
    view: &crate::model::DiffView,
) -> Result<HashSet<Vec<u8>>, EngineError> {
    let mut marked = HashSet::new();
    let paths: Vec<&[u8]> = view.files.iter().map(|f| f.path.as_slice()).collect();
    for attr in &config.attributes {
        for answer in git.check_attr(attr, &paths)? {
            // Which attributes to ask about is config's business; what the
            // answers mean is domain policy.
            if plan::attr_marks_generated(&answer.value) {
                marked.insert(answer.path);
            }
        }
    }
    Ok(marked)
}
