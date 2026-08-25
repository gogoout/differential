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
use std::ffi::OsStr;

use differential_engine::schema;

use differential_engine::EngineError;
use differential_engine::apply::apply_hunks;
use differential_engine::gitio::Repo;
use differential_engine::invariants::dumb_hunk_count;
use differential_engine::model::{DiffView, Disposition};
use differential_engine::plan::{self, Deferral, Fold, HunkId, PlanIndex, reading_split};
use differential_engine::tree::{index_entry, removal_entry};

/// Ref-name component width (`spec/stack.md`).
///
/// Narrower than `plan::short_oid` on purpose: this one ends up in a ref a
/// human types back, so it is a documented part of the CLI contract rather
/// than a display convenience.
const REF_ABBREV: usize = 7;

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
pub fn build_stack(
    repo: &Repo,
    doc: &schema::PlanDocument,
    view: &DiffView,
    opts: &StackOptions,
) -> Result<StackResult, EngineError> {
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

    let (commits, tip) = emit(repo, base, head, view, &plan)?;

    // Invariant 3: the tip tree, built from applied hunks, equals head's tree.
    let tip_tree = repo.rev_parse_raw(&format!("{tip}^{{tree}}"))?;
    let head_tree = repo.rev_parse_raw(&format!("{head}^{{tree}}"))?;
    if tip_tree != head_tree {
        return Err(EngineError::Invariant(format!(
            "stack tip tree {tip_tree} != head tree {head_tree} — a hunk was not carried"
        )));
    }

    // Invariant 4: independent recount over the built commits.
    let mut recount = 0usize;
    let mut parent = base.clone();
    for c in &commits {
        let patch = repo.run(
            ["diff-tree", "-r", "-U0", "--no-renames", &parent, &c.sha],
            None,
        )?;
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
    repo.run(["update-ref", &ref_name, &tip], None)?;

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

    // The audit's back-fill group is assembled last and the ordering stage
    // keeps it trailing, so its position identifies it.
    let backfilled = doc.audit.classes_missing.unwrap_or(0) > 0;
    let mut plan = Vec::new();

    for (gi, g) in groups.iter().enumerate() {
        // Always folded: the stack's way of unfolding a skim group is the
        // [skim 2/2] commit that follows it.
        let split = reading_split(&index, g, Fold::Folded);
        let body = format!("{}\n\n{}", g.description, g.reason);
        let is_backfill = backfilled && gi == groups.len() - 1;

        match split.deferral {
            Deferral::None if is_backfill => plan.push(PlannedCommit {
                subject: format!(
                    "[unclassified] {} hunks carried by no group",
                    split.shown.len()
                ),
                body,
                hunks: split.shown,
                meta_files: Vec::new(),
            }),
            Deferral::None if g.effort == schema::Effort::Skim => plan.push(PlannedCommit {
                subject: format!("[skim] {} — {} exemplars", g.label, split.shown.len()),
                body: format!("{body}\n\nEvery shape class in this group is a singleton."),
                hunks: split.shown,
                meta_files: Vec::new(),
            }),
            Deferral::None => plan.push(PlannedCommit {
                subject: format!("[{}] {}", plan::effort_name(g.effort), g.label),
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
fn emit(
    repo: &Repo,
    base: &str,
    head: &str,
    view: &DiffView,
    plan: &[PlannedCommit],
) -> Result<(Vec<StackCommit>, String), EngineError> {
    let idx = tempfile::NamedTempFile::new().map_err(|e| EngineError::GitSpawn { source: e })?;
    let env: [(&str, &OsStr); 5] = [
        ("GIT_INDEX_FILE", idx.path().as_os_str()),
        ("GIT_AUTHOR_NAME", OsStr::new("differential")),
        ("GIT_AUTHOR_EMAIL", OsStr::new("differential@localhost")),
        ("GIT_COMMITTER_NAME", OsStr::new("differential")),
        ("GIT_COMMITTER_EMAIL", OsStr::new("differential@localhost")),
    ];
    repo.run_env(["read-tree", base], None, &env)?;

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

        let mut feed: Vec<u8> = Vec::new();
        for &fi in &touched {
            feed.extend_from_slice(&stage_file(
                repo,
                base,
                view,
                fi,
                &applied,
                &mut base_blobs,
            )?);
            feed.push(0);
        }
        for &fi in &c.meta_files {
            let f = &view.files[fi];
            let entry = if f.disposition == Disposition::Deleted {
                removal_entry(&f.path)
            } else {
                let mode = f.new_mode.as_deref().ok_or_else(|| missing_mode(f))?;
                let oid = f.new_oid.as_deref().ok_or_else(|| {
                    EngineError::Invariant(format!(
                        "zero-hunk file {} has no recorded oid",
                        String::from_utf8_lossy(&f.path)
                    ))
                })?;
                index_entry(mode, oid, &f.path)
            };
            feed.extend_from_slice(&entry);
            feed.push(0);
        }
        if !feed.is_empty() {
            repo.run_env(["update-index", "-z", "--index-info"], Some(&feed), &env)?;
        }

        let tree = String::from_utf8_lossy(&repo.run_env(["write-tree"], None, &env)?)
            .trim()
            .to_string();
        let msg = format!("{}\n\n{}\n\n{}\n", c.subject, c.body, trailer);
        let sha = String::from_utf8_lossy(&repo.run_env(
            ["commit-tree", &tree, "-p", &parent],
            Some(msg.as_bytes()),
            &env,
        )?)
        .trim()
        .to_string();
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
fn stage_file(
    repo: &Repo,
    base: &str,
    view: &DiffView,
    fi: usize,
    applied: &HashMap<usize, Vec<usize>>,
    base_blobs: &mut HashMap<usize, Option<Vec<u8>>>,
) -> Result<Vec<u8>, EngineError> {
    let f = &view.files[fi];
    let done = applied.get(&fi).map_or(0, Vec::len) == f.hunks.len();

    if f.disposition == Disposition::Deleted && done {
        return Ok(removal_entry(&f.path));
    }
    if let Some((_, new)) = &f.submodule {
        let oid = new.as_deref().or(f.new_oid.as_deref()).ok_or_else(|| {
            EngineError::Invariant(format!(
                "submodule {} has no new commit id",
                String::from_utf8_lossy(&f.path)
            ))
        })?;
        return Ok(index_entry("160000", oid, &f.path));
    }

    let mode = f
        .new_mode
        .as_deref()
        .or(f.old_mode.as_deref())
        .ok_or_else(|| missing_mode(f))?;
    if let std::collections::hash_map::Entry::Vacant(e) = base_blobs.entry(fi) {
        e.insert(repo.blob(base, &f.path)?);
    }
    let hunks: Vec<&differential_engine::model::Hunk> = applied
        .get(&fi)
        .map(|v| v.iter().map(|&h| &view.hunks[h]).collect())
        .unwrap_or_default();
    let content = apply_hunks(base_blobs[&fi].as_deref(), &hunks);
    let out = repo.run(["hash-object", "-w", "--stdin"], Some(&content))?;
    let oid = String::from_utf8_lossy(&out).trim().to_string();
    Ok(index_entry(mode, &oid, &f.path))
}

fn missing_mode(f: &differential_engine::model::FileChange) -> EngineError {
    EngineError::Invariant(format!(
        "no mode recorded for {}",
        String::from_utf8_lossy(&f.path)
    ))
}

/// Output of the full stack pipeline.
pub struct StackOutput {
    pub pipeline: differential_engine::PipelineOutput,
    /// `None` iff invariants failed upstream (no document, nothing rendered).
    pub stack: Option<StackResult>,
}

/// Full production path for the shadow-branch renderer: grouped pipeline
/// (core -> group -> order, in the engine) -> commit stack.
// Mirrors run_grouped_pipeline plus the stack options; bundling the two option
// structs further would be indirection for the lint's sake.
#[allow(clippy::too_many_arguments)]
pub fn run_stack_pipeline(
    repo: &Repo,
    base_rev: &str,
    head_rev: &str,
    kind: schema::SourceKind,
    config: &differential_engine::config::Config,
    langs: &differential_engine::lang::LanguageRegistry,
    grouping: &differential_engine::grouping::GroupingOptions,
    stack: &StackOptions,
) -> Result<StackOutput, EngineError> {
    let out = differential_engine::run_grouped_pipeline(
        repo, base_rev, head_rev, kind, config, langs, grouping,
    )?;
    let Some(doc) = &out.document else {
        return Ok(StackOutput {
            pipeline: out,
            stack: None,
        });
    };
    let result = build_stack(repo, doc, &out.view, stack)?;
    Ok(StackOutput {
        pipeline: out,
        stack: Some(result),
    })
}
