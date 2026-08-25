//! The effort-tier reading rule (ADR 0006), in one place.
//!
//! What a reviewer is asked to read in a group, and what is deliberately
//! withheld, is domain policy — but it was implemented twice, nearly
//! character-for-character, in the TUI's row builder and the stack renderer.
//! Two copies of a rule is two rules; these two had already drifted apart on
//! how they treat a back-filled group.

use crate::plan::HunkId;
use crate::plan::ids::PlanIndex;
use crate::schema;

/// Whether the deferrable half of a group is currently hidden.
///
/// The TUI toggles this with `z`. The stack is always `Folded`: its way of
/// unfolding is the next commit in the series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fold {
    Folded,
    Unfolded,
}

/// Why the deferred half is deferred. A renderer's fold line or commit subject
/// depends on this and on nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deferral {
    /// Nothing is withheld: the focus tier, an unfolded group, or a skim group
    /// whose every shape class is a singleton (so the exemplars *are* the
    /// group).
    None,
    /// Remaining members of the shapes the shown exemplars verify.
    SkimRemainder,
    /// Generated content, folded whole — there is no exemplar worth reading.
    FoldedNoise,
}

/// One group split into what to read and what to defer.
///
/// Both halves are in class order (`class_ids` order, then `hunk_ids` order
/// within a class), which is the order both renderers already emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingSplit {
    pub shown: Vec<HunkId>,
    pub deferred: Vec<HunkId>,
    pub deferral: Deferral,
}

impl ReadingSplit {
    /// Every hunk the group carries, shown first.
    ///
    /// The stack needs this: a noise group is still one commit carrying every
    /// hunk, even though a reviewer is asked to read none of them.
    pub fn all(&self) -> Vec<HunkId> {
        let mut out = self.shown.clone();
        out.extend_from_slice(&self.deferred);
        out
    }
}

/// Split a group into its read and deferred halves.
///
/// - focus: everything is read.
/// - skim, folded: one exemplar per shape class; the rest is deferred, because
///   verifying the exemplar verifies the shape.
/// - noise, folded: nothing is read.
/// - anything unfolded: everything is read.
pub fn reading_split(index: &PlanIndex, group: &schema::Group, fold: Fold) -> ReadingSplit {
    let everything = || ReadingSplit {
        shown: index.group_hunks(group),
        deferred: Vec::new(),
        deferral: Deferral::None,
    };

    if fold == Fold::Unfolded {
        return everything();
    }

    match group.effort {
        schema::Effort::Focus => everything(),

        schema::Effort::Noise => ReadingSplit {
            shown: Vec::new(),
            deferred: index.group_hunks(group),
            deferral: Deferral::FoldedNoise,
        },

        schema::Effort::Skim => {
            let shown: Vec<HunkId> = group.class_ids.iter().map(|c| index.exemplar(c)).collect();
            let deferred: Vec<HunkId> = group
                .class_ids
                .iter()
                .flat_map(|c| {
                    let exemplar = index.exemplar(c);
                    index
                        .class_hunks(c)
                        .into_iter()
                        .filter(move |h| *h != exemplar)
                })
                .collect();
            // Singleton classes leave nothing to defer, and a renderer must
            // not offer to unfold an empty remainder.
            let deferral = if deferred.is_empty() {
                Deferral::None
            } else {
                Deferral::SkimRemainder
            };
            ReadingSplit {
                shown,
                deferred,
                deferral,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::test_support::{doc_with, group, hunk_ids};

    /// Two classes: a 3-member shape and a singleton.
    fn two_classes() -> schema::PlanDocument {
        doc_with(
            &[("C0", &["h0", "h1", "h2"], "h0"), ("C1", &["h3"], "h3")],
            &[],
        )
    }

    fn split(effort: schema::Effort, fold: Fold) -> ReadingSplit {
        let mut doc = two_classes();
        doc.groups = Some(vec![group("g0", effort, &["C0", "C1"])]);
        let index = PlanIndex::build(&doc).unwrap();
        let groups = index.groups();
        reading_split(&index, &groups[0], fold)
    }

    #[test]
    fn focus_reads_everything_folded_or_not() {
        for fold in [Fold::Folded, Fold::Unfolded] {
            let s = split(schema::Effort::Focus, fold);
            assert_eq!(hunk_ids(&s.shown), ["h0", "h1", "h2", "h3"]);
            assert!(s.deferred.is_empty());
            assert_eq!(s.deferral, Deferral::None);
        }
    }

    #[test]
    fn folded_skim_shows_one_exemplar_per_class_and_defers_the_rest() {
        let s = split(schema::Effort::Skim, Fold::Folded);
        assert_eq!(hunk_ids(&s.shown), ["h0", "h3"]);
        assert_eq!(hunk_ids(&s.deferred), ["h1", "h2"]);
        assert_eq!(s.deferral, Deferral::SkimRemainder);
    }

    #[test]
    fn folded_noise_defers_everything_and_shows_nothing() {
        let s = split(schema::Effort::Noise, Fold::Folded);
        assert!(s.shown.is_empty());
        assert_eq!(hunk_ids(&s.deferred), ["h0", "h1", "h2", "h3"]);
        assert_eq!(s.deferral, Deferral::FoldedNoise);
    }

    #[test]
    fn unfolding_reads_everything_whatever_the_tier() {
        for effort in [schema::Effort::Skim, schema::Effort::Noise] {
            let s = split(effort, Fold::Unfolded);
            assert_eq!(hunk_ids(&s.shown), ["h0", "h1", "h2", "h3"]);
            assert_eq!(s.deferral, Deferral::None);
        }
    }

    /// A skim group of singletons has nothing behind the fold, so a renderer
    /// must not offer to unfold one.
    #[test]
    fn a_skim_group_of_singletons_defers_nothing() {
        let mut doc = doc_with(&[("C0", &["h0"], "h0"), ("C1", &["h1"], "h1")], &[]);
        doc.groups = Some(vec![group("g0", schema::Effort::Skim, &["C0", "C1"])]);
        let index = PlanIndex::build(&doc).unwrap();
        let s = reading_split(&index, &index.groups()[0], Fold::Folded);

        assert_eq!(hunk_ids(&s.shown), ["h0", "h1"]);
        assert!(s.deferred.is_empty());
        assert_eq!(
            s.deferral,
            Deferral::None,
            "nothing is withheld, so there is nothing to unfold"
        );
    }

    /// The stack commits every hunk in a noise group even though a reviewer
    /// reads none of them — coverage is structural, not a function of effort.
    #[test]
    fn all_carries_every_hunk_shown_first() {
        let s = split(schema::Effort::Skim, Fold::Folded);
        assert_eq!(hunk_ids(&s.all()), ["h0", "h3", "h1", "h2"]);

        let noise = split(schema::Effort::Noise, Fold::Folded);
        assert_eq!(hunk_ids(&noise.all()), ["h0", "h1", "h2", "h3"]);
    }
}
