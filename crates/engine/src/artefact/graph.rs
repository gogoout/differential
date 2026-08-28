//! The class dependency graph: definition → use edges between shape classes.
//!
//! Built once, from classes, before the model runs (ADR 0022). Two consumers
//! read it: the artefact the model fetches from, and the ordering stage, which
//! contracts it onto groups.
//!
//! **It is a fact about the diff, not about the grouping.** The stage that used
//! to build it worked from groups, so a symbol two classes defined produced an
//! edge only when the model happened to merge those two classes. What depends
//! on what cannot turn on how a label was drawn.
//!
//! Extraction is heuristic, per-language via the `Language` hooks (ADR 0015);
//! no indexer. Precision is allowed to be low (ADR 0007): a wrong edge
//! misorders, and it can never hide content. Every edge carries the symbols
//! that produced it, so a consumer can judge one by its cause rather than take
//! it on trust.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::lang::LanguageRegistry;
use crate::model::DiffView;
use crate::schema;
use crate::shape::Partition;

/// What each class introduces, and which classes it consumes. Both indexed by
/// class index, parallel to `Partition::classes`.
pub struct ClassGraph {
    pub defines: Vec<Vec<String>>,
    pub depends_on: Vec<Vec<schema::ClassEdge>>,
}

/// Build the graph over the **added** lines of every class: what the change
/// introduces, and what the changed code now calls.
///
/// Hunks in generated files contribute no symbols. A lockfile would otherwise
/// appear to define half the dependency tree. This is classification, never
/// enumeration — the class, its hunks and its files all still exist
/// (ADR 0005/0012).
pub fn build(view: &DiffView, partition: &Partition, langs: &LanguageRegistry) -> ClassGraph {
    let n = partition.classes.len();
    let mut defs: Vec<BTreeSet<Vec<u8>>> = vec![BTreeSet::new(); n];
    let mut refs: Vec<BTreeSet<Vec<u8>>> = vec![BTreeSet::new(); n];

    for (ci, members) in partition.classes.iter().enumerate() {
        for &hi in members {
            let h = &view.hunks[hi];
            let file = view.file_of(h);
            if file.generated.is_some() {
                continue;
            }
            let lang = langs.detect(&file.path);
            for line in &h.added {
                defs[ci].extend(lang.symbol_definitions(line));
                refs[ci].extend(lang.symbol_references(line));
            }
        }
    }

    // Only symbols defined by exactly ONE class create edges. A symbol two
    // classes define is ambiguous, and this heuristic cannot say which one a
    // reference meant; a precise `Language` (ADR 0015) would resolve it
    // instead of dropping it.
    let mut definer: HashMap<&[u8], Option<usize>> = HashMap::new();
    for (ci, d) in defs.iter().enumerate() {
        for sym in d {
            definer
                .entry(sym.as_slice())
                .and_modify(|e| *e = None)
                .or_insert(Some(ci));
        }
    }

    let mut depends_on: Vec<Vec<schema::ClassEdge>> = Vec::with_capacity(n);
    for (ci, r) in refs.iter().enumerate() {
        // BTreeMap keyed by the defining class index: edges come out sorted by
        // class number, which is `C0`, `C1`, … in the ids too.
        let mut by_target: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for sym in r {
            if let Some(&Some(def_ci)) = definer.get(sym.as_slice())
                && def_ci != ci
            {
                by_target.entry(def_ci).or_default().push(text(sym));
            }
        }
        depends_on.push(
            by_target
                .into_iter()
                .map(|(target, via)| schema::ClassEdge {
                    on: format!("C{target}"),
                    via,
                })
                .collect(),
        );
    }

    ClassGraph {
        defines: defs
            .iter()
            .map(|d| d.iter().map(|s| text(s)).collect())
            .collect(),
        depends_on,
    }
}

/// Symbols reach the schema as text. They are identifiers by construction, so
/// this is the display boundary and lossy conversion is the honest answer to
/// bytes that are not.
fn text(sym: &[u8]) -> String {
    String::from_utf8_lossy(sym).into_owned()
}
