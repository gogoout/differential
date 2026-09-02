//! The shadow-branch renderer: the diff rewritten as a synthetic commit stack
//! on a `refs/review/…/stack` ref, one commit per group in reading order —
//! `git log --oneline` alone shows the shape of the change, and skim
//! remainders are skippable on their subject line.
//!
//! Plumbing only (ADR 0011): temp index, hash-object, update-index,
//! write-tree, commit-tree, update-ref. No checkout, no branch switch, no
//! contact with the user's worktree.
//!
//! Every commit's content is computed BY APPLYING HUNKS cumulatively — the
//! final tip tree equalling the head tree is therefore a real assertion that
//! every hunk was carried (invariant 3), backed by an independent per-commit
//! `@@` recount (invariant 4). The exceptions are the same documented ones as
//! the core tree builder: zero-hunk files (binary, mode-only, empty) are
//! staged from recorded oids in a trailing `[meta]` commit.

use std::collections::HashMap;

use differential_engine::schema;

use differential_engine::EngineError;
use differential_engine::apply::apply_hunks;
use differential_engine::invariants::dumb_hunk_count;
use differential_engine::model::DiffView;
use differential_engine::plan::{self, Deferral, Fold, HunkId, PlanIndex, reading_split};
use differential_engine::ports::{
    AttributeSource, CommitIdentity, CommitWriter, DiffSource, IndexEntry, IndexSession,
    ObjectReader, ObjectWriter, RangeResolver, RecountSource, RefWriter, TreeBuilder, TreeResolver,
};

/// Ref-name component width (`spec/stack.md`).
///
/// Narrower than `plan::short_oid` on purpose: this one ends up in a ref a
/// human types back, so it is a documented part of the CLI contract rather
/// than a display convenience.
const REF_ABBREV: usize = 7;

/// Who the synthetic commits belong to. Fixed, so a re-run of the same range
/// produces the same shas.
const IDENTITY: CommitIdentity<'static> = CommitIdentity {
    name: "differential",
    email: "differential@localhost",
};

#[derive(Default)]
pub struct StackOptions<'a> {
    /// Ref to land the stack on. Default: `refs/review/<base7>-<head7>/stack`.
    pub ref_name: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct StackCommit {
    pub sha: String,
    pub subject: String,
    pub hunks: usize,
}

#[derive(Debug, Clone)]
pub struct StackResult {
    pub ref_name: String,
    pub tip: String,
    pub commits: Vec<StackCommit>,
    pub hunks_carried: usize,
    /// Independent per-commit `@@` recount, summed (invariant 4).
    pub recount: usize,
}

struct PlannedCommit {
    subject: String,
    body: String,
    /// Canonical hunks carried by this commit.
    hunks: Vec<HunkId>,
    /// Zero-hunk file indices carried by this commit (the `[meta]` commit).
    meta_files: Vec<usize>,
}

/// Build and land the stack. Errors (and leaves the ref untouched) if any
/// stack invariant fails.
pub fn build_stack<G>(
    git: &G,
    doc: &schema::PlanDocument,
    view: &DiffView,
    opts: &StackOptions,
) -> Result<StackResult, EngineError>
where
    G: ObjectReader
        + ObjectWriter
        + TreeBuilder
        + CommitWriter
        + TreeResolver
        + RecountSource
        + RefWriter,
{
    let base = &doc.source.base;
    let head = &doc.source.head;
    let mut plan = commit_plan(doc)?;

    // Zero-hunk files (binary, mode-only, empty add/delete) belong to no class
    // and therefore no group; without this commit the tree assertion cannot
    // hold. Staged from recorded oids — the documented tautology.
    let meta_files: Vec<usize> = (0..view.files.len())
        .filter(|&i| view.files[i].hunks.is_empty())
        .collect();
    if !meta_files.is_empty() {
        plan.push(PlannedCommit {
            subject: format!(
                "[meta] {} binary, mode or empty-file changes",
                meta_files.len()
            ),
            body: "Changes that carry no text hunks: binary content, mode-only flips and \
                   empty files. Staged from recorded object ids."
                .to_string(),
            hunks: Vec::new(),
            meta_files,
        });
    }

    // Invariant 2 over the plan: every canonical hunk in exactly one commit.
    let mut seen = vec![false; view.hunks.len()];
    for c in &plan {
        for &h in &c.hunks {
            if seen[h.index()] {
                return Err(EngineError::Invariant(format!(
                    "hunk {h} carried by two commits"
                )));
            }
            seen[h.index()] = true;
        }
    }
    let hunks_carried = seen.iter().filter(|s| **s).count();
    if hunks_carried != view.hunks.len() {
        return Err(EngineError::Invariant(format!(
            "stack plan carries {hunks_carried} hunks, {} exist",
            view.hunks.len()
        )));
    }

    let (commits, tip) = emit(git, base, head, view, &plan)?;

    // Invariant 3: the tip tree, built from applied hunks, equals head's tree.
    let tip_tree = git.tree_of(&tip)?;
    let head_tree = git.tree_of(head)?;
    if tip_tree != head_tree {
        return Err(EngineError::Invariant(format!(
            "stack tip tree {tip_tree} != head tree {head_tree} — a hunk was not carried"
        )));
    }

    // Invariant 4: independent recount over the built commits.
    let mut recount = 0usize;
    let mut parent = base.clone();
    for c in &commits {
        let patch = git.recount_patch(&parent, &c.sha)?;
        recount += dumb_hunk_count(&patch);
        parent = c.sha.clone();
    }
    if recount != view.hunks.len() {
        return Err(EngineError::Invariant(format!(
            "stack recount {recount} != canonical {}",
            view.hunks.len()
        )));
    }

    let ref_name = opts.ref_name.map(str::to_string).unwrap_or_else(|| {
        format!(
            "refs/review/{}-{}/stack",
            &base[..REF_ABBREV.min(base.len())],
            &head[..REF_ABBREV.min(head.len())]
        )
    });
    git.update_ref(&ref_name, &tip)?;

    Ok(StackResult {
        ref_name,
        tip,
        commits,
        hunks_carried,
        recount,
    })
}

/// One commit per group in rank order; skim groups split into exemplars (one
/// hunk per shape class) and a remainder skippable on its subject line.
fn commit_plan(doc: &schema::PlanDocument) -> Result<Vec<PlannedCommit>, EngineError> {
    let Some(groups) = &doc.groups else {
        return Err(EngineError::Invariant(
            "stack rendering needs a grouped document (groups is null)".into(),
        ));
    };
    let index = PlanIndex::build(doc)?;
    // Which group is the audit's back-fill, and what token names a tier, are
    // both domain answers. They used to be re-derived here — the back-fill
    // test character for character, the token by composing `effort_name`
    // itself — which is exactly the drift `plan::ReviewView` exists to stop.
    let review = plan::ReviewView::project(doc)?;
    let mut plan = Vec::new();

    for (g, gv) in groups.iter().zip(&review.groups) {
        // Always folded: the stack's way of unfolding a skim group is the
        // [skim 2/2] commit that follows it.
        let split = reading_split(&index, g, Fold::Folded);
        let body = format!("{}\n\n{}", g.description, g.reason);
        // `unclassified` for the back-fill, the tier's own name otherwise.
        let tier = review.tier_name(gv);

        match split.deferral {
            Deferral::None if gv.unclassified => plan.push(PlannedCommit {
                subject: format!("[{tier}] {} hunks carried by no group", split.shown.len()),
                body,
                hunks: split.shown,
                meta_files: Vec::new(),
            }),
            Deferral::None if g.effort == schema::Effort::Skim => plan.push(PlannedCommit {
                subject: format!("[{tier}] {} — {} exemplars", g.label, split.shown.len()),
                body: format!("{body}\n\nEvery shape class in this group is a singleton."),
                hunks: split.shown,
                meta_files: Vec::new(),
            }),
            Deferral::None => plan.push(PlannedCommit {
                subject: format!("[{tier}] {}", g.label),
                body,
                hunks: split.shown,
                meta_files: Vec::new(),
            }),
            Deferral::FoldedNoise => plan.push(PlannedCommit {
                subject: format!(
                    "[noise] {} — folded, {} hunks",
                    g.label,
                    split.deferred.len()
                ),
                body,
                // A folded group still carries every hunk: what a reviewer is
                // asked to read never decides what the commit contains.
                hunks: split.all(),
                meta_files: Vec::new(),
            }),
            Deferral::SkimRemainder => {
                plan.push(PlannedCommit {
                    subject: format!("[skim 1/2] {} — {} exemplars", g.label, split.shown.len()),
                    body: format!(
                        "{body}\n\nOne hunk per shape class. {} further hunks follow in \
                         [skim 2/2].",
                        split.deferred.len()
                    ),
                    hunks: split.shown,
                    meta_files: Vec::new(),
                });
                plan.push(PlannedCommit {
                    subject: format!(
                        "[skim 2/2] {} — {} further hunks, same shapes",
                        g.label,
                        split.deferred.len()
                    ),
                    body: "Remaining members of the shapes verified in [skim 1/2]. \
                           Skippable on this subject line."
                        .to_string(),
                    hunks: split.deferred,
                    meta_files: Vec::new(),
                });
            }
        }
    }
    Ok(plan)
}

/// Cumulative emission over a temporary index.
fn emit<G>(
    git: &G,
    base: &str,
    head: &str,
    view: &DiffView,
    plan: &[PlannedCommit],
) -> Result<(Vec<StackCommit>, String), EngineError>
where
    G: ObjectReader + ObjectWriter + TreeBuilder + CommitWriter,
{
    let mut session = git.begin_from_tree(base)?;

    let mut applied: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut base_blobs: HashMap<usize, Option<Vec<u8>>> = HashMap::new();
    let mut parent = base.to_string();
    let mut commits = Vec::with_capacity(plan.len());
    let trailer = format!(
        "Review-Synthetic: {}..{}",
        plan::short_oid(base),
        plan::short_oid(head)
    );

    for c in plan {
        let mut touched: Vec<usize> = c
            .hunks
            .iter()
            .map(|&h| view.hunks[h.index()].file)
            .collect();
        touched.sort_unstable();
        touched.dedup();
        for &h in &c.hunks {
            applied
                .entry(view.hunks[h.index()].file)
                .or_default()
                .push(h.index());
        }

        let mut entries: Vec<IndexEntry> = Vec::new();
        for &fi in &touched {
            entries.push(stage_file(git, base, view, fi, &applied, &mut base_blobs)?);
        }
        for &fi in &c.meta_files {
            // What a zero-hunk file contributes is a domain rule, and this
            // loop used to state it again in its own words — deletion, then
            // the recorded mode and oid, then the same two error strings.
            let f = &view.files[fi];
            entries.push(match plan::zero_hunk_state(f)? {
                plan::Staged::Remove => IndexEntry::Remove {
                    path: f.path.clone(),
                },
                plan::Staged::Recorded { mode, oid } => IndexEntry::Set {
                    mode: mode.to_string(),
                    oid: oid.to_string(),
                    path: f.path.clone(),
                },
                // The rule never answers this, and an error says so where a
                // panic would only assert it.
                plan::Staged::Apply { .. } => {
                    return Err(EngineError::Invariant(format!(
                        "zero-hunk file {} was asked to apply hunks it has none of",
                        String::from_utf8_lossy(&f.path)
                    )));
                }
            });
        }
        session.stage(&entries)?;

        let tree = session.write_tree()?;
        let msg = format!("{}\n\n{}\n\n{}\n", c.subject, c.body, trailer);
        // The synthetic identity is the renderer's policy, expressed as data
        // rather than as an environment the whole session inherits.
        let sha = git.commit_tree(&tree, &parent, msg.as_bytes(), IDENTITY)?;
        commits.push(StackCommit {
            sha: sha.clone(),
            subject: c.subject.clone(),
            hunks: c.hunks.len(),
        });
        parent = sha;
    }
    Ok((commits, parent))
}

/// Stage one file's cumulative state: full-application deletions become
/// removals; everything else is content computed by applying the hunks seen so
/// far (submodules become gitlinks from the pseudo-hunk's commit id).
fn stage_file<G>(
    git: &G,
    base: &str,
    view: &DiffView,
    fi: usize,
    applied: &HashMap<usize, Vec<usize>>,
    base_blobs: &mut HashMap<usize, Option<Vec<u8>>>,
) -> Result<IndexEntry, EngineError>
where
    G: ObjectReader + ObjectWriter,
{
    let f = &view.files[fi];
    let applied_here = applied.get(&fi).map_or(0, Vec::len);

    match plan::cumulative_state(f, applied_here)? {
        plan::Staged::Remove => Ok(IndexEntry::Remove {
            path: f.path.clone(),
        }),
        plan::Staged::Recorded { mode, oid } => Ok(IndexEntry::Set {
            mode: mode.to_string(),
            oid: oid.to_string(),
            path: f.path.clone(),
        }),
        plan::Staged::Apply { mode } => {
            if let std::collections::hash_map::Entry::Vacant(e) = base_blobs.entry(fi) {
                e.insert(git.blob(base, &f.path)?);
            }
            let hunks: Vec<&differential_engine::model::Hunk> = applied
                .get(&fi)
                .map(|v| v.iter().map(|&h| &view.hunks[h]).collect())
                .unwrap_or_default();
            let content = apply_hunks(base_blobs[&fi].as_deref(), &hunks);
            Ok(IndexEntry::Set {
                mode: mode.to_string(),
                oid: git.write_blob(&content)?,
                path: f.path.clone(),
            })
        }
    }
}

/// Output of the full stack pipeline.
pub struct StackOutput {
    pub pipeline: differential_engine::PipelineOutput,
    /// `None` iff invariants failed upstream (no document, nothing rendered).
    pub stack: Option<StackResult>,
}

/// Full production path for the shadow-branch renderer: grouped pipeline
/// (core -> group -> order, in the engine) -> commit stack.
pub fn run_stack_pipeline<G, C, A>(
    git: &G,
    source: &plan::ReviewSource,
    config: &differential_engine::config::Config,
    langs: &differential_engine::lang::LanguageRegistry,
    symbols: &differential_engine::artefact::symbols::SymbolReaders,
    grouping: &differential_engine::grouping::GroupingOptions<C, A>,
    stack: &StackOptions,
) -> Result<StackOutput, EngineError>
where
    G: ObjectReader
        + ObjectWriter
        + TreeBuilder
        + CommitWriter
        + TreeResolver
        + RecountSource
        + RefWriter
        + RangeResolver
        + DiffSource
        + AttributeSource,
    C: differential_engine::ports::GroupingCache,
    A: differential_engine::ports::ArtefactStore,
{
    let mut out = differential_engine::run_grouped_pipeline(
        git,
        &source.base,
        &source.head,
        source.kind,
        config,
        langs,
        symbols,
        grouping,
    )?;
    // Invariants 3 and 4. This renderer is the reason they exist: its commits
    // are trees built from these hunks, so the tree assertion is about exactly
    // the path taken below. The engine's pipeline no longer runs them, because
    // a consumer that never builds a tree is not protected by them.
    differential_engine::verify(git, &mut out)?;
    if !out.report.all_ok() {
        return Ok(StackOutput {
            pipeline: out,
            stack: None,
        });
    }
    let Some(doc) = &out.document else {
        return Ok(StackOutput {
            pipeline: out,
            stack: None,
        });
    };
    let result = build_stack(git, doc, &out.view, stack)?;
    Ok(StackOutput {
        pipeline: out,
        stack: Some(result),
    })
}
