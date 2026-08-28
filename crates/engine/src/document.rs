//! Assemble the frozen-schema plan document from the engine's outputs.

use std::collections::HashSet;

use crate::schema;

use crate::EngineError;
use crate::artefact::graph::ClassGraph;
use crate::config::Config;
use crate::invariants::InvariantReport;
use crate::model::{DiffView, Disposition, GeneratedBy};
use crate::shape::{Partition, hunk_digest};

/// Built-in generated-artefact detection: lockfiles and minified/snapshot
/// artefacts nobody reviews line by line. A hint only — never affects
/// enumeration (ADR 0005/0012), and `not_generated` in config overrides it.
pub fn builtin_generated(path: &[u8]) -> bool {
    let base = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
    const NAMES: &[&[u8]] = &[
        b"Cargo.lock",
        b"package-lock.json",
        b"pnpm-lock.yaml",
        b"yarn.lock",
        b"bun.lockb",
        b"composer.lock",
        b"Gemfile.lock",
        b"poetry.lock",
        b"uv.lock",
        b"go.sum",
        b"flake.lock",
    ];
    if NAMES.contains(&base) {
        return true;
    }
    const SUFFIXES: &[&[u8]] = &[b".lock", b".snap", b".min.js", b".min.css"];
    SUFFIXES.iter().any(|s| base.ends_with(s))
}

/// Apply generated hints with provenance. Precedence: `not_generated` clears
/// everything; otherwise config glob > gitattributes > builtin.
pub fn mark_generated(view: &mut DiffView, config: &Config, attr_marked: &HashSet<Vec<u8>>) {
    for f in &mut view.files {
        let lossy = String::from_utf8_lossy(&f.path).into_owned();
        if config.not_generated.is_match(&lossy) {
            f.generated = None;
        } else if config.generated.is_match(&lossy) {
            f.generated = Some(GeneratedBy::Config);
        } else if attr_marked.contains(&f.path) {
            f.generated = Some(GeneratedBy::Attr);
        } else if builtin_generated(&f.path) {
            f.generated = Some(GeneratedBy::Builtin);
        } else {
            f.generated = None;
        }
    }
}

pub struct SourceInfo {
    pub kind: schema::SourceKind,
    pub base: String,
    pub head: String,
}

/// Build the document. Fails on a non-UTF-8 path (deferred support — a hard
/// error naming the file, never silent).
pub fn assemble(
    view: &DiffView,
    partition: &Partition,
    graph: ClassGraph,
    source: &SourceInfo,
    report: &InvariantReport,
) -> Result<schema::PlanDocument, EngineError> {
    let files = view
        .files
        .iter()
        .map(|f| {
            Ok(schema::FileEntry {
                path: path_str(&f.path)?,
                disposition: match f.disposition {
                    Disposition::Added => schema::Disposition::A,
                    Disposition::Deleted => schema::Disposition::D,
                    Disposition::Modified => schema::Disposition::M,
                },
                mode: f.new_mode.clone(),
                old_mode: f.old_mode.clone(),
                old_path: f.rename_from.as_deref().map(path_str).transpose()?,
                new_path: f.rename_to.as_deref().map(path_str).transpose()?,
                rename_similarity: f.rename_similarity,
                binary: f.binary,
                submodule: f
                    .submodule
                    .as_ref()
                    .map(|(old, new)| schema::SubmoduleChange {
                        old: old.clone(),
                        new: new.clone(),
                    }),
                generated: f.generated.is_some(),
                generated_by: f.generated.map(|g| match g {
                    GeneratedBy::Builtin => schema::GeneratedBy::Builtin,
                    GeneratedBy::Attr => schema::GeneratedBy::Attr,
                    GeneratedBy::Config => schema::GeneratedBy::Config,
                }),
                hunk_ids: f.hunks.iter().map(|i| format!("h{i}")).collect(),
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;

    let hunks = view
        .hunks
        .iter()
        .enumerate()
        .map(|(i, h)| {
            Ok(schema::HunkEntry {
                id: format!("h{i}"),
                file: path_str(&view.file_of(h).path)?,
                old_start: h.old_start,
                old_count: h.old_count,
                new_start: h.new_start,
                new_count: h.new_count,
                class: format!("C{}", partition.class_of[i]),
                digest: hunk_digest(h),
                nonl_old: h.nonl_old,
                nonl_new: h.nonl_new,
                forge_position: schema::ForgePosition {
                    new_line: (h.new_count > 0).then_some(h.new_start),
                    old_line: (h.old_count > 0).then_some(h.old_start),
                },
            })
        })
        .collect::<Result<Vec<_>, EngineError>>()?;

    // The graph arrives owned: its two vectors are moved into the classes
    // rather than cloned, and a class is the only place either belongs.
    let mut graph = graph;
    let classes = partition
        .classes
        .iter()
        .enumerate()
        .map(|(ci, members)| schema::ClassEntry {
            id: format!("C{ci}"),
            hunk_ids: members.iter().map(|i| format!("h{i}")).collect(),
            exemplar: format!("h{}", members[0]),
            pure_substitution: partition.pure[ci],
            defines: std::mem::take(&mut graph.defines[ci]),
            depends_on: std::mem::take(&mut graph.depends_on[ci]),
        })
        .collect::<Vec<_>>();

    Ok(schema::PlanDocument {
        schema_version: schema::SCHEMA_VERSION,
        generator: schema::Generator {
            tool: "differential".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            stages: vec!["enumerate".to_string(), "classify".to_string()],
        },
        source: schema::Source {
            kind: source.kind,
            base: source.base.clone(),
            head: source.head.clone(),
            remote: None,
        },
        stats: schema::Stats {
            files: view.files.len() as u32,
            hunks: view.hunks.len() as u32,
            classes: partition.classes.len() as u32,
            binary_files: view.files.iter().filter(|f| f.binary).count() as u32,
            submodules: view.files.iter().filter(|f| f.submodule.is_some()).count() as u32,
        },
        files,
        hunks,
        classes,
        groups: None,
        reading_plan: None,
        audit: schema::Audit {
            applier_exact: report.applier_exact(),
            tree_assertion: if report.tree_ok { "pass" } else { "fail" }.to_string(),
            hunks_carried: report.hunks_total as u32,
            recount: report.recount as u32,
            coverage: None,
            classes_missing: None,
            classes_duplicated: None,
            classes_hallucinated: None,
            read_hunks: None,
            skipped_hunks: None,
        },
    })
}

fn path_str(path: &[u8]) -> Result<String, EngineError> {
    String::from_utf8(path.to_vec()).map_err(|_| EngineError::NonUtf8Path {
        lossy: String::from_utf8_lossy(path).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::builtin_generated;

    #[test]
    fn builtin_list_matches() {
        assert!(builtin_generated(b"Cargo.lock"));
        assert!(builtin_generated(b"nested/dir/package-lock.json"));
        assert!(builtin_generated(b"ui/__snapshots__/thing.snap"));
        assert!(builtin_generated(b"dist/app.min.js"));
        assert!(!builtin_generated(b"src/main.rs"));
        assert!(!builtin_generated(b"docs/lockfile-design.md"));
    }
}
