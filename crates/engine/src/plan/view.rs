//! A renderer-agnostic projection of one plan document.
//!
//! The arithmetic every reviewer surface needs — group totals, file totals,
//! resolved dependency edges, reviewed-mark keys — computed once, in the
//! domain. It lived in the TUI's constructor, which is why the stack had to
//! re-derive its own half and why the two drifted.

use std::collections::HashMap;

use crate::EngineError;
use crate::plan::identity::class_content_key;
use crate::plan::ids::{HunkId, PlanIndex};
use crate::plan::{LineCounts, effort_name};
use crate::schema;

/// One resolved `depends_on` edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    pub id: String,
    pub label: String,
    /// The dependency appears **later** in the plan.
    ///
    /// Which means the two groups depend on each other and the topological
    /// sort had to break the cycle. The plan says so rather than quietly
    /// presenting an order it could not honour.
    pub unsatisfied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupView {
    pub id: String,
    pub label: String,
    pub effort: schema::Effort,
    pub role: Option<schema::Role>,
    pub class_ids: Vec<String>,
    /// Class content keys — the reviewed-mark keys — in `class_ids` order.
    pub class_keys: Vec<String>,
    /// Members in class order.
    pub hunks: Vec<HunkId>,
    /// Distinct paths touched. A rename counts twice, because the canonical
    /// view is `--no-renames`; zero-hunk changes contribute nothing.
    pub n_files: usize,
    pub counts: LineCounts,
    pub depends_on: Vec<Dependency>,
    /// The audit's back-fill: classes the model omitted, recovered by the
    /// coverage audit and read last (ADR 0001, invariant 5).
    pub unclassified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileView {
    pub path: String,
    /// Canonical hunks, file order.
    pub hunks: Vec<HunkId>,
    pub counts: LineCounts,
}

/// The projection. Owned, not borrowing the document, so a session can hold
/// both without becoming self-referential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewView {
    pub groups: Vec<GroupView>,
    /// Every file in the document, document order — including the zero-hunk
    /// binary, submodule and mode-only changes the group view cannot surface.
    pub files: Vec<FileView>,
    group_of_hunk: HashMap<HunkId, usize>,
    hunk_by_digest: HashMap<String, HunkId>,
    class_key: HashMap<String, String>,
    hunk_key: HashMap<HunkId, String>,
}

impl ReviewView {
    /// Project a document, validating it on the way through.
    ///
    /// Needs no store and therefore no port: reviewed-mark keys are a pure
    /// function of the hunk digests the document already carries, which is
    /// why `ReviewSession` can stop computing its own copy of them.
    pub fn project(doc: &schema::PlanDocument) -> Result<Self, EngineError> {
        let index = PlanIndex::build(doc)?;

        let mut class_key = HashMap::new();
        let mut hunk_key = HashMap::new();
        for c in &doc.classes {
            let members = index.class_hunks(&c.id);
            let digests: Vec<String> = members
                .iter()
                .map(|&h| index.hunk(h).digest.clone())
                .collect();
            let key = class_content_key(&digests);
            for h in &members {
                hunk_key.insert(*h, key.clone());
            }
            class_key.insert(c.id.clone(), key);
        }

        let groups = index.groups();
        let label_of: HashMap<&str, &str> = groups
            .iter()
            .map(|g| (g.id.as_str(), g.label.as_str()))
            .collect();
        let rank_of: HashMap<&str, usize> = groups
            .iter()
            .enumerate()
            .map(|(i, g)| (g.id.as_str(), i))
            .collect();

        // The back-fill group is assembled last and the ordering stage keeps
        // it trailing, so its position identifies it. Positional — but
        // positional in ONE place, instead of once per renderer.
        let backfilled = doc.audit.classes_missing.unwrap_or(0) > 0;

        let projected: Vec<GroupView> = groups
            .iter()
            .enumerate()
            .map(|(rank, g)| {
                let hunks = index.group_hunks(g);
                let files: std::collections::HashSet<&str> =
                    hunks.iter().map(|&h| index.hunk(h).file.as_str()).collect();
                GroupView {
                    id: g.id.clone(),
                    label: g.label.clone(),
                    effort: g.effort,
                    role: g.role,
                    class_keys: g
                        .class_ids
                        .iter()
                        .map(|c| class_key[c.as_str()].clone())
                        .collect(),
                    class_ids: g.class_ids.clone(),
                    n_files: files.len(),
                    counts: hunks
                        .iter()
                        .map(|&h| LineCounts::of_hunk(index.hunk(h)))
                        .sum(),
                    depends_on: g
                        .depends_on
                        .iter()
                        .map(|id| Dependency {
                            label: label_of
                                .get(id.as_str())
                                .map(|l| (*l).to_string())
                                .unwrap_or_else(|| id.clone()),
                            unsatisfied: rank_of.get(id.as_str()).copied().unwrap_or(0) > rank,
                            id: id.clone(),
                        })
                        .collect(),
                    unclassified: backfilled && rank + 1 == groups.len(),
                    hunks,
                }
            })
            .collect();

        let mut group_of_hunk = HashMap::new();
        for (i, g) in projected.iter().enumerate() {
            for &h in &g.hunks {
                group_of_hunk.insert(h, i);
            }
        }

        let files: Vec<FileView> = doc
            .files
            .iter()
            .map(|f| {
                let hunks = index.file_hunks(f);
                FileView {
                    path: f.path.clone(),
                    counts: hunks
                        .iter()
                        .map(|&h| LineCounts::of_hunk(index.hunk(h)))
                        .sum(),
                    hunks,
                }
            })
            .collect();

        let hunk_by_digest = doc
            .hunks
            .iter()
            .enumerate()
            .map(|(i, h)| (h.digest.clone(), HunkId::from_index(i)))
            .collect();

        Ok(ReviewView {
            groups: projected,
            files,
            group_of_hunk,
            hunk_by_digest,
            class_key,
            hunk_key,
        })
    }

    pub fn group_position(&self, id: &str) -> Option<usize> {
        self.groups.iter().position(|g| g.id == id)
    }

    pub fn group_by_id(&self, id: &str) -> Option<&GroupView> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// The group owning a hunk, via its class.
    ///
    /// `None` only for a hunk whose class is in no group — impossible after
    /// the coverage audit, but the type says so rather than a comment.
    pub fn group_of_hunk(&self, hunk: HunkId) -> Option<&GroupView> {
        self.group_of_hunk.get(&hunk).map(|&i| &self.groups[i])
    }

    /// Findings anchor on digests, which survive regeneration where positional
    /// ids do not.
    pub fn hunk_by_digest(&self, digest: &str) -> Option<HunkId> {
        self.hunk_by_digest.get(digest).copied()
    }

    pub fn class_key(&self, class_id: &str) -> &str {
        &self.class_key[class_id]
    }

    pub fn hunk_key(&self, hunk: HunkId) -> &str {
        &self.hunk_key[&hunk]
    }

    /// Hunks whose class is marked reviewed.
    pub fn hunks_with_keys<'k>(
        &self,
        reviewed: impl Fn(&str) -> bool + 'k,
    ) -> std::collections::HashSet<HunkId> {
        self.hunk_key
            .iter()
            .filter(|(_, key)| reviewed(key))
            .map(|(h, _)| *h)
            .collect()
    }

    /// The tier's domain name, or `unclassified` for the audit back-fill.
    ///
    /// One answer for both renderers: the stack has always labelled the
    /// back-fill distinctly and the TUI has always shown it as an ordinary
    /// focus group, which is the same document described two ways.
    pub fn tier_name(&self, group: &GroupView) -> &'static str {
        if group.unclassified {
            "unclassified"
        } else {
            effort_name(group.effort)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::test_support::{doc_with, group, hunk_ids};

    fn two_group_doc() -> schema::PlanDocument {
        let mut doc = doc_with(
            &[("C0", &["h0", "h1"], "h0"), ("C1", &["h2"], "h2")],
            &[("src/a.rs", &["h0", "h1"]), ("src/b.rs", &["h2"])],
        );
        doc.groups = Some(vec![
            group("g0", schema::Effort::Focus, &["C0"]),
            group("g1", schema::Effort::Skim, &["C1"]),
        ]);
        doc
    }

    #[test]
    fn groups_carry_their_totals_and_distinct_file_count() {
        let doc = two_group_doc();
        let view = ReviewView::project(&doc).unwrap();

        assert_eq!(hunk_ids(&view.groups[0].hunks), ["h0", "h1"]);
        assert_eq!(
            view.groups[0].n_files, 2,
            "the fixture puts each hunk in its own file"
        );
        // The fixture's hunks are +2/-1 each.
        assert_eq!(view.groups[0].counts, LineCounts { adds: 4, dels: 2 });
        assert_eq!(view.files[0].counts, LineCounts { adds: 4, dels: 2 });
    }

    #[test]
    fn reviewed_keys_are_a_pure_function_of_the_documents_digests() {
        let doc = two_group_doc();
        let view = ReviewView::project(&doc).unwrap();

        // Same arithmetic ReviewSession used to do for itself.
        let expected = class_content_key(&["digest0".into(), "digest1".into()]);
        assert_eq!(view.class_key("C0"), expected);
        assert_eq!(view.hunk_key(HunkId::from_index(0)), expected);
        assert_eq!(view.hunk_key(HunkId::from_index(1)), expected);
        assert_ne!(view.class_key("C1"), expected);
    }

    #[test]
    fn a_dependency_listed_later_is_flagged_unsatisfied() {
        let mut doc = two_group_doc();
        // g0 (rank 0) depends on g1 (rank 1): the order could not honour it.
        doc.groups.as_mut().unwrap()[0].depends_on = vec!["g1".into()];
        doc.groups.as_mut().unwrap()[1].depends_on = vec!["g0".into()];
        let view = ReviewView::project(&doc).unwrap();

        assert_eq!(
            view.groups[0].depends_on,
            [Dependency {
                id: "g1".into(),
                label: "g1 label".into(),
                unsatisfied: true
            }]
        );
        assert!(
            !view.groups[1].depends_on[0].unsatisfied,
            "a dependency earlier in the plan is honoured"
        );
    }

    #[test]
    fn hunks_resolve_to_their_owning_group_and_their_digest() {
        let doc = two_group_doc();
        let view = ReviewView::project(&doc).unwrap();

        assert_eq!(view.group_of_hunk(HunkId::from_index(1)).unwrap().id, "g0");
        assert_eq!(view.hunk_by_digest("digest2"), Some(HunkId::from_index(2)));
        assert_eq!(view.hunk_by_digest("nope"), None);
    }

    /// The asymmetry this projection exists to remove: one flag, so a renderer
    /// cannot decide for itself that a back-filled group is ordinary.
    #[test]
    fn the_trailing_backfill_group_is_marked_unclassified() {
        let mut doc = two_group_doc();
        assert!(
            ReviewView::project(&doc)
                .unwrap()
                .groups
                .iter()
                .all(|g| !g.unclassified),
            "no back-fill recorded in the audit"
        );

        doc.audit.classes_missing = Some(1);
        let view = ReviewView::project(&doc).unwrap();
        assert!(!view.groups[0].unclassified);
        assert!(
            view.groups[1].unclassified,
            "the back-fill is assembled last"
        );
        assert_eq!(view.tier_name(&view.groups[1]), "unclassified");
        assert_eq!(view.tier_name(&view.groups[0]), "focus");
    }
}
