//! Domain policy over a plan document: the decisions, without the I/O.
//!
//! Everything here is pure and consumes only `schema`. It exists because the
//! same decisions were being made independently in `crates/tui` and
//! `crates/stack` — and had already drifted (ADR 0020).

mod counts;
mod enumerate;
mod identity;
mod ids;
mod source;
mod staging;
mod tiers;
mod view;

pub use counts::LineCounts;
pub use enumerate::{Enumeration, attr_marks_generated, build_view, classify};
pub use identity::{
    artefact_dir, class_content_key, grouping_cache_dir, plan_hash, review_dir, review_id,
};
pub use ids::{HunkId, PlanIndex};
pub use source::{RangeSpec, ReviewSource, parse_range};
pub use staging::{Staged, cumulative_state, final_state};
pub use tiers::{Deferral, Fold, ReadingSplit, class_is_generated, reading_split};
pub use view::{Dependency, FileView, GroupView, ReviewView};

use crate::schema;

/// The tier's domain name, identical to its wire value.
///
/// Renderers compose their own vocabulary from this — a commit subject, a
/// glyph, a colour — but they all start from one token, so none of them can
/// drift away from the schema on its own.
pub const fn effort_name(effort: schema::Effort) -> &'static str {
    match effort {
        schema::Effort::Focus => "focus",
        schema::Effort::Skim => "skim",
        schema::Effort::Noise => "noise",
    }
}

/// The ordering role's domain name, identical to its wire value.
pub const fn role_name(role: schema::Role) -> &'static str {
    match role {
        schema::Role::Foundation => "foundation",
        schema::Role::Consumer => "consumer",
        schema::Role::Mechanical => "mechanical",
        schema::Role::Noise => "noise",
    }
}

/// How many hex characters of an oid to show a human.
const SHORT_OID: usize = 12;

/// An oid abbreviated for display.
///
/// Deliberately not `git rev-parse --short`: no uniqueness check and no repo
/// access, because every call site wanted a fixed prefix to print, not an
/// abbreviation a reader could resolve. An oid that must be typed *back* is a
/// different problem — the stack's ref name uses its own narrower width, which
/// `spec/stack.md` documents.
pub fn short_oid(oid: &str) -> &str {
    &oid[..SHORT_OID.min(oid.len())]
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Hand-built documents for the pure tests: no repo, no pipeline.

    use crate::schema;

    use super::HunkId;

    /// A document with `classes` (id, member ids, exemplar id) and `files`
    /// (path, member ids). Hunks are synthesized to cover every id mentioned.
    pub fn doc_with(
        classes: &[(&str, &[&str], &str)],
        files: &[(&str, &[&str])],
    ) -> schema::PlanDocument {
        let n = classes
            .iter()
            .flat_map(|(_, ids, _)| ids.iter())
            .chain(files.iter().flat_map(|(_, ids)| ids.iter()))
            .filter_map(|id| id.strip_prefix('h').and_then(|n| n.parse::<usize>().ok()))
            .map(|i| i + 1)
            .max()
            .unwrap_or(0);

        schema::PlanDocument {
            schema_version: schema::SCHEMA_VERSION,
            generator: schema::Generator {
                tool: "test".into(),
                version: "0".into(),
                stages: vec![],
            },
            source: schema::Source {
                kind: schema::SourceKind::Range,
                base: "0".repeat(40),
                head: "1".repeat(40),
                remote: None,
            },
            stats: schema::Stats {
                files: files.len() as u32,
                hunks: n as u32,
                classes: classes.len() as u32,
                binary_files: 0,
                submodules: 0,
            },
            files: files
                .iter()
                .map(|(path, ids)| file_entry(path, ids))
                .collect(),
            hunks: (0..n).map(hunk_entry).collect(),
            classes: classes
                .iter()
                .map(|(id, ids, exemplar)| schema::ClassEntry {
                    id: (*id).into(),
                    hunk_ids: ids.iter().map(|s| (*s).into()).collect(),
                    exemplar: (*exemplar).into(),
                    pure_substitution: false,
                    defines: vec![],
                    depends_on: vec![],
                })
                .collect(),
            groups: None,
            reading_plan: None,
            audit: audit(),
        }
    }

    pub fn group(id: &str, effort: schema::Effort, class_ids: &[&str]) -> schema::Group {
        schema::Group {
            id: id.into(),
            label: format!("{id} label"),
            description: "d".into(),
            reason: "r".into(),
            effort,
            role: None,
            class_ids: class_ids.iter().map(|s| (*s).into()).collect(),
            depends_on: vec![],
            rank: 0,
            pivot: None,
        }
    }

    /// Render ids back to the wire form, so a failing assertion reads as the
    /// ids a document actually contains.
    pub fn hunk_ids(hunks: &[HunkId]) -> Vec<String> {
        hunks.iter().map(HunkId::to_string).collect()
    }

    fn hunk_entry(i: usize) -> schema::HunkEntry {
        schema::HunkEntry {
            id: format!("h{i}"),
            file: format!("src/f{i}.rs"),
            old_start: i as u32 + 1,
            old_count: 1,
            new_start: i as u32 + 1,
            new_count: 2,
            class: String::new(),
            digest: format!("digest{i}"),
            nonl_old: false,
            nonl_new: false,
            forge_position: schema::ForgePosition {
                new_line: Some(i as u32 + 1),
                old_line: Some(i as u32 + 1),
            },
        }
    }

    fn file_entry(path: &str, ids: &[&str]) -> schema::FileEntry {
        schema::FileEntry {
            path: path.into(),
            disposition: schema::Disposition::M,
            mode: Some("100644".into()),
            old_mode: None,
            old_path: None,
            new_path: None,
            rename_similarity: None,
            binary: false,
            submodule: None,
            generated: false,
            generated_by: None,
            hunk_ids: ids.iter().map(|s| (*s).into()).collect(),
        }
    }

    fn audit() -> schema::Audit {
        schema::Audit {
            applier_exact: "0/0".into(),
            tree_assertion: "pass".into(),
            hunks_carried: 0,
            recount: 0,
            coverage: None,
            classes_missing: None,
            classes_duplicated: None,
            classes_hallucinated: None,
            read_hunks: None,
            skipped_hunks: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_and_role_names_are_the_wire_values() {
        // If these ever diverge from serde, a renderer composing `[focus]`
        // and a document saying `"skim"` would disagree silently.
        for effort in [
            schema::Effort::Focus,
            schema::Effort::Skim,
            schema::Effort::Noise,
        ] {
            let wire = serde_json::to_string(&effort).unwrap();
            assert_eq!(wire, format!("\"{}\"", effort_name(effort)));
        }
        for role in [
            schema::Role::Foundation,
            schema::Role::Consumer,
            schema::Role::Mechanical,
            schema::Role::Noise,
        ] {
            let wire = serde_json::to_string(&role).unwrap();
            assert_eq!(wire, format!("\"{}\"", role_name(role)));
        }
    }

    #[test]
    fn short_oid_truncates_and_tolerates_short_input() {
        assert_eq!(short_oid("0123456789abcdef0123"), "0123456789ab");
        assert_eq!(short_oid("abc"), "abc");
        assert_eq!(short_oid(""), "");
    }
}
