//! Hunk identity, and a validated index over a plan document.

use std::collections::HashMap;

use crate::EngineError;
use crate::schema;

/// A canonical hunk, in memory.
///
/// The wire form is `h<N>`, where N indexes `doc.hunks` — frozen since schema
/// v1 and unchanged by this type. `HunkId` is only ever the parsed form: it
/// never crosses a serde boundary, so the contract stays exactly as
/// `spec/json-contract.md` describes it while callers stop re-parsing strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HunkId(usize);

impl HunkId {
    pub const fn from_index(index: usize) -> Self {
        HunkId(index)
    }

    pub const fn index(self) -> usize {
        self.0
    }

    /// Parse the wire form.
    ///
    /// Fallible on purpose. A malformed id means the document contradicts its
    /// own contract, which is impossible for a document this process just
    /// produced and possible for one read back from a review store — so the
    /// caller gets an error to propagate instead of a panic.
    pub fn parse(s: &str) -> Result<Self, EngineError> {
        s.strip_prefix('h')
            .and_then(|n| n.parse().ok())
            .map(HunkId)
            .ok_or_else(|| {
                EngineError::PlanIntegrity(format!("malformed hunk id {s:?}; expected h<N>"))
            })
    }
}

impl std::fmt::Display for HunkId {
    /// Writes the wire form, so `format!("{id}")` round-trips through `parse`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "h{}", self.0)
    }
}

/// A plan document's id lookups, built and validated once.
///
/// Transient by construction: it borrows the document, so it is built where it
/// is used and dropped there. That is already what every hand-rolled copy of
/// this map did — the difference is that the ids are checked on the way in, so
/// every accessor below is total and none of them can panic.
pub struct PlanIndex<'d> {
    doc: &'d schema::PlanDocument,
    class_by_id: HashMap<&'d str, &'d schema::ClassEntry>,
}

impl<'d> PlanIndex<'d> {
    /// Build, validating every id the document refers to: hunk ids parse and
    /// are in range, class ids referenced by groups exist.
    ///
    /// Before this existed the two renderers disagreed about a broken
    /// document — `crates/stack` panicked on an unresolvable class reference
    /// and the TUI silently dropped it, so a corrupt store showed a short
    /// group with no indication anything was missing.
    pub fn build(doc: &'d schema::PlanDocument) -> Result<Self, EngineError> {
        let class_by_id: HashMap<&str, &schema::ClassEntry> =
            doc.classes.iter().map(|c| (c.id.as_str(), c)).collect();

        let n = doc.hunks.len();
        let check = |hid: &str| -> Result<(), EngineError> {
            let h = HunkId::parse(hid)?;
            if h.index() >= n {
                return Err(EngineError::PlanIntegrity(format!(
                    "hunk id {hid} is out of range: the document has {n} hunks"
                )));
            }
            Ok(())
        };

        for c in &doc.classes {
            check(&c.exemplar)?;
            for hid in &c.hunk_ids {
                check(hid)?;
            }
        }
        for f in &doc.files {
            for hid in &f.hunk_ids {
                check(hid)?;
            }
        }
        for g in doc.groups.iter().flatten() {
            for cid in &g.class_ids {
                if !class_by_id.contains_key(cid.as_str()) {
                    return Err(EngineError::PlanIntegrity(format!(
                        "group {} references class {cid}, which the document does not define",
                        g.id
                    )));
                }
            }
        }

        Ok(PlanIndex { doc, class_by_id })
    }

    pub fn doc(&self) -> &'d schema::PlanDocument {
        self.doc
    }

    /// The document's groups, or an empty slice for a core-only document.
    pub fn groups(&self) -> &'d [schema::Group] {
        self.doc.groups.as_deref().unwrap_or(&[])
    }

    /// Total: `build` proved every referenced class id resolves.
    pub fn class(&self, id: &str) -> &'d schema::ClassEntry {
        self.class_by_id[id]
    }

    /// Total: `build` proved every hunk id is in range.
    pub fn hunk(&self, h: HunkId) -> &'d schema::HunkEntry {
        &self.doc.hunks[h.index()]
    }

    pub fn exemplar(&self, class_id: &str) -> HunkId {
        self.parsed(&self.class(class_id).exemplar)
    }

    pub fn class_hunks(&self, class_id: &str) -> Vec<HunkId> {
        self.class(class_id)
            .hunk_ids
            .iter()
            .map(|h| self.parsed(h))
            .collect()
    }

    /// Group members in class order — the order both renderers already emit.
    pub fn group_hunks(&self, group: &schema::Group) -> Vec<HunkId> {
        group
            .class_ids
            .iter()
            .flat_map(|c| self.class_hunks(c))
            .collect()
    }

    pub fn file_hunks(&self, file: &schema::FileEntry) -> Vec<HunkId> {
        file.hunk_ids.iter().map(|h| self.parsed(h)).collect()
    }

    /// Infallible re-parse of an id `build` already validated.
    fn parsed(&self, hid: &str) -> HunkId {
        HunkId::parse(hid).expect("PlanIndex::build validated every id in the document")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_ids_round_trip_through_the_wire_form() {
        for n in [0usize, 1, 42, 1234] {
            let id = HunkId::from_index(n);
            assert_eq!(id.to_string(), format!("h{n}"));
            assert_eq!(HunkId::parse(&id.to_string()).unwrap(), id);
        }
    }

    /// Both renderers used to answer a broken document differently — the stack
    /// panicked, the TUI dropped the reference and rendered a short group with
    /// no sign anything was missing. One error, raised once, replaces both.
    #[test]
    fn build_rejects_a_document_that_contradicts_itself() {
        use crate::plan::test_support::{doc_with, group};

        // `doc_with` synthesizes a hunk for every id mentioned, so the range
        // has to be broken deliberately after the fact.
        let mut out_of_range = doc_with(&[("C0", &["h0", "h9"], "h0")], &[]);
        out_of_range.hunks.truncate(1);
        assert!(
            matches!(
                PlanIndex::build(&out_of_range),
                Err(EngineError::PlanIntegrity(_))
            ),
            "a member id past the end of hunks must not reach an accessor"
        );

        let mut missing_class = doc_with(&[("C0", &["h0"], "h0")], &[]);
        missing_class.groups = Some(vec![group("g0", schema::Effort::Focus, &["C0", "C7"])]);
        assert!(matches!(
            PlanIndex::build(&missing_class),
            Err(EngineError::PlanIntegrity(_))
        ));

        let bad_exemplar = doc_with(&[("C0", &["h0"], "nope")], &[]);
        assert!(matches!(
            PlanIndex::build(&bad_exemplar),
            Err(EngineError::PlanIntegrity(_))
        ));
    }

    /// Every accessor is total once `build` returns, which is what lets the
    /// call sites drop their `expect`s.
    #[test]
    fn accessors_are_total_after_a_successful_build() {
        use crate::plan::test_support::{doc_with, group};

        let mut doc = doc_with(
            &[("C0", &["h0", "h1"], "h0"), ("C1", &["h2"], "h2")],
            &[("src/a.rs", &["h0", "h1"])],
        );
        doc.groups = Some(vec![group("g0", schema::Effort::Focus, &["C0", "C1"])]);
        let index = PlanIndex::build(&doc).unwrap();

        assert_eq!(index.exemplar("C0"), HunkId::from_index(0));
        assert_eq!(index.class_hunks("C1"), [HunkId::from_index(2)]);
        assert_eq!(index.hunk(HunkId::from_index(1)).id, "h1");
        assert_eq!(index.group_hunks(&index.groups()[0]).len(), 3);
        assert_eq!(index.file_hunks(&doc.files[0]).len(), 2);
    }

    /// A core-only document has no groups; that is a state, not a failure.
    #[test]
    fn an_ungrouped_document_indexes_with_no_groups() {
        let doc = crate::plan::test_support::doc_with(&[("C0", &["h0"], "h0")], &[]);
        let index = PlanIndex::build(&doc).unwrap();
        assert!(index.groups().is_empty());
    }

    #[test]
    fn malformed_hunk_ids_are_rejected_not_panicked_on() {
        // The last two are why this is not `hid[1..].parse()`: slicing a
        // multi-byte first character panics, and a bare "h" slices to "".
        for bad in ["", "x0", "h", "h-1", "hfoo", "0", "é0"] {
            let err = HunkId::parse(bad).unwrap_err();
            assert!(
                matches!(err, EngineError::PlanIntegrity(_)),
                "{bad:?} produced {err:?}"
            );
        }
    }
}
