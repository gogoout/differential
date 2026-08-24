//! The grouping stage: an LLM merges and labels shape-class ids — never hunks
//! (ADR 0001) — behind the `LlmBackend` abstraction (ADR 0016), with a coverage
//! audit that back-fills anything the model drops (invariant 5) and a
//! content-hash cache that pins groupings (ADR 0009).
//!
//! Mechanical pieces the model never sees or cannot override:
//! - classes living entirely in generated files are pre-assigned to the noise
//!   tier and never reach the payload (ADR 0006);
//! - classes touching a rename below 95% similarity can never stay in a skim
//!   group — they are extracted into a synthesized focus group (ADR 0003).

mod assemble;
mod cache;
mod parse;
mod payload;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::llm::LlmBackend;
use crate::schema;

use crate::EngineError;
use crate::model::DiffView;

pub use payload::PROMPT_VERSION;

pub struct GroupingOptions<'a> {
    /// Injected backend; `None` lets the pipeline build one from
    /// `[grouping].command` (default: the validated claude invocation).
    pub backend: Option<&'a dyn LlmBackend>,
    /// Cache directory (spec/persistence.md suggests
    /// `<git-common-dir>/differential/cache/grouping`). `None` disables caching.
    pub cache_dir: Option<&'a Path>,
    /// Stage notifications for renderers that show progress while the
    /// pipeline runs (the TUI's splash screen). `None` reports nothing.
    pub progress: Option<&'a (dyn Fn(Progress) + Send + Sync)>,
}

/// Pipeline stage notifications, in the order they occur. `Grouping` carries
/// the backend name so a renderer can say WHICH agent it is waiting on — that
/// stage is the slow one (a subprocess LLM call on a cache miss).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Enumerating,
    Classifying,
    Grouping { backend: String, cached: bool },
    Ordering,
    Done,
}

/// Everything the stage needs to know about one shape class, derived from the
/// core document + the diff view.
pub(crate) struct ClassInfo {
    pub id: String,
    pub n_hunks: usize,
    /// Sorted unique file paths touched by members.
    pub files: Vec<String>,
    pub kind: char,
    /// Exemplar hunk index into `view.hunks`.
    pub exemplar: usize,
    /// Every member hunk lives in a generated file → noise tier.
    pub all_generated: bool,
    /// Some member touches a rename below the relocation threshold.
    pub rename_gated: bool,
    /// "renamed from <old>, <sim>% similar" annotation for the payload.
    pub rename_note: Option<String>,
    /// Sorted member digests, for the cache key.
    pub digests: Vec<String>,
}

/// A group between audit and assembly.
pub(crate) struct WorkGroup {
    pub label: String,
    pub description: String,
    pub reason: String,
    pub skim: bool,
    pub class_ids: Vec<String>,
    pub backfill: bool,
}

const RELOCATION_THRESHOLD: u8 = 95;

/// Run the grouping stage over a core-only document. Returns the same document
/// with `groups`, `reading_plan` and the grouping audit fields filled, and
/// `"group"` appended to `generator.stages`.
pub fn run(
    doc: &schema::PlanDocument,
    view: &DiffView,
    backend: &dyn LlmBackend,
    cache_dir: Option<&Path>,
    lang_fingerprint: &str,
    progress: Option<&(dyn Fn(Progress) + Send + Sync)>,
) -> Result<schema::PlanDocument, EngineError> {
    let infos = class_infos(doc);

    let (noise, offered): (Vec<&ClassInfo>, Vec<&ClassInfo>) =
        infos.iter().partition(|c| c.all_generated);

    let mut audited = if offered.is_empty() {
        Audited {
            groups: Vec::new(),
            missing: Vec::new(),
            dupes: Vec::new(),
            halluc: Vec::new(),
            coverage: 1.0,
        }
    } else {
        let prompt = payload::build_prompt(&offered, view);
        let response = fetch_response(
            &prompt,
            &offered,
            backend,
            cache_dir,
            lang_fingerprint,
            progress,
        )?;
        let raw = parse::parse_response(&response)?;
        audit(raw, &offered)
    };

    apply_relocation_gate(&mut audited.groups, &infos);

    Ok(assemble::assemble(doc, &infos, &noise, audited))
}

pub(crate) struct Audited {
    pub groups: Vec<WorkGroup>,
    pub missing: Vec<String>,
    pub dupes: Vec<String>,
    pub halluc: Vec<String>,
    /// Model-assigned hunks / offered hunks, pre-back-fill. The honest number.
    pub coverage: f64,
}

/// The coverage audit — the whole point of merging class ids instead of hunk
/// indices (ADR 0001): an omitted id is detectable and back-filled, never lost.
fn audit(raw: parse::RawGroups, offered: &[&ClassInfo]) -> Audited {
    let known: HashMap<&str, &ClassInfo> = offered.iter().map(|c| (c.id.as_str(), *c)).collect();

    let mut claimed: HashSet<String> = HashSet::new();
    let mut dupes = Vec::new();
    let mut halluc = Vec::new();
    let mut groups = Vec::new();

    for g in raw.groups {
        let mut kept = Vec::new();
        for cid in g.classes {
            if !known.contains_key(cid.as_str()) {
                if !halluc.contains(&cid) {
                    halluc.push(cid);
                }
            } else if claimed.contains(&cid) {
                if !dupes.contains(&cid) {
                    dupes.push(cid);
                }
            } else {
                claimed.insert(cid.clone());
                kept.push(cid);
            }
        }
        if !kept.is_empty() {
            groups.push(WorkGroup {
                label: g.label,
                description: g.description,
                reason: g.reason,
                skim: g.effort == "skim",
                class_ids: kept,
                backfill: false,
            });
        }
    }

    let missing: Vec<String> = offered
        .iter()
        .filter(|c| !claimed.contains(&c.id))
        .map(|c| c.id.clone())
        .collect();

    let offered_hunks: usize = offered.iter().map(|c| c.n_hunks).sum();
    let assigned_hunks: usize = offered
        .iter()
        .filter(|c| claimed.contains(&c.id))
        .map(|c| c.n_hunks)
        .sum();
    let coverage = if offered_hunks == 0 {
        1.0
    } else {
        assigned_hunks as f64 / offered_hunks as f64
    };

    if !missing.is_empty() {
        groups.push(WorkGroup {
            label: "Carried by no group".to_string(),
            description: "Classes the model omitted; recovered by the coverage audit.".to_string(),
            reason: "Not triaged — must be read.".to_string(),
            skim: false,
            class_ids: missing.clone(),
            backfill: true,
        });
    }

    Audited {
        groups,
        missing,
        dupes,
        halluc,
        coverage,
    }
}

/// ADR 0003: a class touching a sub-threshold rename is a modification, not a
/// relocation, and can never stay in a skim group. Deterministic backstop that
/// runs after the audit, whatever the model claimed.
fn apply_relocation_gate(groups: &mut Vec<WorkGroup>, infos: &[ClassInfo]) {
    let gated: HashSet<&str> = infos
        .iter()
        .filter(|c| c.rename_gated)
        .map(|c| c.id.as_str())
        .collect();
    if gated.is_empty() {
        return;
    }

    let mut extracted = Vec::new();
    for g in groups.iter_mut() {
        if !g.skim {
            continue;
        }
        let (out, kept): (Vec<String>, Vec<String>) = g
            .class_ids
            .drain(..)
            .partition(|cid| gated.contains(cid.as_str()));
        g.class_ids = kept;
        extracted.extend(out);
    }
    groups.retain(|g| !g.class_ids.is_empty());

    if !extracted.is_empty() {
        groups.push(WorkGroup {
            label: "Modified during move".to_string(),
            description: format!(
                "Renamed files below the {RELOCATION_THRESHOLD}% relocation threshold: \
                 rewritten during the move, not relocated verbatim."
            ),
            reason: "Rename-similarity gate: a low-similarity rename is a modification and \
                     is never skim-eligible."
                .to_string(),
            skim: false,
            class_ids: extracted,
            backfill: false,
        });
    }
}

/// Derive per-class facts from the core document + view.
fn class_infos(doc: &schema::PlanDocument) -> Vec<ClassInfo> {
    let file_by_path: HashMap<&str, &schema::FileEntry> =
        doc.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let hunk_by_id: HashMap<&str, (usize, &schema::HunkEntry)> = doc
        .hunks
        .iter()
        .enumerate()
        .map(|(i, h)| (h.id.as_str(), (i, h)))
        .collect();

    doc.classes
        .iter()
        .map(|c| {
            let members: Vec<(usize, &schema::HunkEntry)> = c
                .hunk_ids
                .iter()
                .map(|hid| hunk_by_id[hid.as_str()])
                .collect();
            let mut files: Vec<String> = members.iter().map(|(_, h)| h.file.clone()).collect();
            files.sort_unstable();
            files.dedup();

            let entries: Vec<&schema::FileEntry> =
                files.iter().map(|p| file_by_path[p.as_str()]).collect();
            let all_generated = entries.iter().all(|f| f.generated);
            let rename_gated = entries.iter().any(|f| {
                f.rename_similarity
                    .is_some_and(|s| s < RELOCATION_THRESHOLD)
            });
            let rename_note = entries.iter().find_map(|f| {
                let sim = f.rename_similarity?;
                let old = f.old_path.as_deref()?;
                Some(format!("renamed from {old}, {sim}% similar"))
            });

            let exemplar_id = c.exemplar.as_str();
            let (exemplar_idx, exemplar_hunk) = hunk_by_id[exemplar_id];
            let kind = match file_by_path[exemplar_hunk.file.as_str()].disposition {
                schema::Disposition::A => 'A',
                schema::Disposition::D => 'D',
                schema::Disposition::M => 'M',
            };

            let mut digests: Vec<String> = members.iter().map(|(_, h)| h.digest.clone()).collect();
            digests.sort_unstable();

            ClassInfo {
                id: c.id.clone(),
                n_hunks: members.len(),
                files,
                kind,
                exemplar: exemplar_idx,
                all_generated,
                rename_gated,
                rename_note,
                digests,
            }
        })
        .collect()
}

/// Cache-or-call: the cached value is the raw model response, so the audit and
/// assembly stay pure functions replayed on every load.
fn fetch_response(
    prompt: &str,
    offered: &[&ClassInfo],
    backend: &dyn LlmBackend,
    cache_dir: Option<&Path>,
    lang_fingerprint: &str,
    progress: Option<&(dyn Fn(Progress) + Send + Sync)>,
) -> Result<String, EngineError> {
    let key = cache::cache_key(offered, backend.name(), lang_fingerprint);
    if let Some(dir) = cache_dir
        && let Some(hit) = cache::load(dir, &key)?
    {
        if let Some(f) = progress {
            f(Progress::Grouping {
                backend: backend.name().to_string(),
                cached: true,
            });
        }
        return Ok(hit);
    }
    if let Some(f) = progress {
        f(Progress::Grouping {
            backend: backend.name().to_string(),
            cached: false,
        });
    }
    let response = backend.complete(prompt)?;
    if let Some(dir) = cache_dir {
        cache::store(dir, &key, &response)?;
    }
    Ok(response)
}
