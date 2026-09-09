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
//! Extraction is a domain use case with pluggable readers ([`super::symbols`]);
//! no indexer. It reads WHOLE FILES from the head tree, because a line inside a
//! block comment cannot be told from code on its own. A file no reader claims
//! contributes nothing — a guess costs more than silence. Precision is allowed to be low (ADR 0007): a wrong edge
//! misorders, and it can never hide content. Every edge carries the symbols
//! that produced it, so a consumer can judge one by its cause rather than take
//! it on trust.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::symbols::{FileSymbols, Scope, Symbol, SymbolReaders};
use crate::EngineError;
use crate::model::DiffView;
use crate::ports::ObjectReader;
use crate::schema;
use crate::shape::Partition;

/// What each class introduces, and which classes it consumes. Both indexed by
/// class index, parallel to `Partition::classes`.
pub struct ClassGraph {
    pub defines: Vec<Vec<String>>,
    pub depends_on: Vec<Vec<schema::ClassEdge>>,
}

/// A symbol as the graph compares it.
///
/// A global name stands alone: `Widget` read from one file is the same
/// `Widget` read from another, which is what lets an edge cross a file. A
/// file-local name carries the index of the file it was read from, so it is
/// only ever equal to itself — `label` in one file and `label` in another are
/// two symbols, and neither can draw an edge to the other's class.
type Key = (Option<usize>, Vec<u8>);

fn key(file: usize, symbol: &Symbol) -> Key {
    match symbol.scope {
        Scope::Global => (None, symbol.name.clone()),
        Scope::File => (Some(file), symbol.name.clone()),
    }
}

/// Build the graph over the **added** lines of every class: what the change
/// introduces, and what the changed code now calls.
///
/// Hunks in generated files contribute no symbols. A lockfile would otherwise
/// appear to define half the dependency tree. This is classification, never
/// enumeration — the class, its hunks and its files all still exist
/// (ADR 0005/0012).
pub fn build<G: ObjectReader>(
    git: &G,
    head: &str,
    view: &DiffView,
    partition: &Partition,
    symbols: &SymbolReaders,
) -> Result<ClassGraph, EngineError> {
    let parsed = parse_files(git, head, view, symbols)?;

    let n = partition.classes.len();
    let mut defs: Vec<BTreeSet<Key>> = vec![BTreeSet::new(); n];
    let mut refs: Vec<BTreeSet<Key>> = vec![BTreeSet::new(); n];

    for (ci, members) in partition.classes.iter().enumerate() {
        for &hi in members {
            let h = &view.hunks[hi];
            let file = view.file_of(h);
            // Neither contributes a symbol, and each for its own reason.
            // Generated content defines nothing — a lockfile would otherwise
            // appear to define half the dependency tree. A gitlink's only added
            // line is `Subproject commit <oid>`: diff prose about a commit this
            // repository does not have, whose words are plausible identifiers.
            //
            // Both skips belong HERE rather than only in `parse_files`. A
            // category excluded from the blob read still reaches the fallback,
            // which is how the gitlink's prose used to become references.
            if file.generated.is_some() || file.submodule.is_some() {
                continue;
            }
            // No entry means no reader claimed the file, or none could read
            // it. Either way the class gains no symbols from this hunk: the
            // domain never substitutes one reader's answer for another's, and
            // never invents one of its own.
            if let Some(fs) = parsed.get(&h.file) {
                for i in 0..h.added.len() {
                    let line = h.new_start + i as u32;
                    defs[ci].extend(fs.defines_at(line).iter().map(|s| key(h.file, s)));
                    refs[ci].extend(fs.references_at(line).iter().map(|s| key(h.file, s)));
                }
            }
        }
    }

    // Only symbols defined by exactly ONE class create edges. A symbol two
    // classes define is ambiguous, and this heuristic cannot say which one a
    // reference meant; a precise `Language` (ADR 0015) would resolve it
    // instead of dropping it.
    //
    // A file-local key carries its file, so the ambiguity is judged per file
    // too: two files each declaring `label` are not a clash, and one file
    // declaring it twice still is.
    let mut definer: HashMap<&Key, Option<usize>> = HashMap::new();
    for (ci, d) in defs.iter().enumerate() {
        for sym in d {
            definer
                .entry(sym)
                .and_modify(|e| *e = None)
                .or_insert(Some(ci));
        }
    }

    let mut depends_on: Vec<Vec<schema::ClassEdge>> = Vec::with_capacity(n);
    for (ci, r) in refs.iter().enumerate() {
        // BTreeMap keyed by the defining class index: edges come out sorted by
        // class number, which is `C0`, `C1`, … in the ids too.
        let mut by_target: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
        for sym in r {
            if let Some(&Some(def_ci)) = definer.get(sym)
                && def_ci != ci
            {
                by_target.entry(def_ci).or_default().insert(text(&sym.1));
            }
        }
        depends_on.push(
            by_target
                .into_iter()
                .map(|(target, via)| schema::ClassEdge {
                    on: format!("C{target}"),
                    via: via.into_iter().collect(),
                })
                .collect(),
        );
    }

    Ok(ClassGraph {
        defines: defs
            .iter()
            // A class can define one name globally and another locally, and
            // could in principle define the same spelling both ways. The set
            // is over the printed name, so the list stays one entry per name.
            .map(|d| {
                d.iter()
                    .map(|k| text(&k.1))
                    .collect::<BTreeSet<String>>()
                    .into_iter()
                    .collect()
            })
            .collect(),
        depends_on,
    })
}

/// Parse every file that can contribute a symbol, once — keyed by file index.
///
/// **Whole files, from the head tree.** The hooks used to see one diff line at
/// a time, which cannot tell a line inside a block comment from code. So the
/// content comes from the odb and the hunks say which of its lines to read.
///
/// One bulk read for the lot: a blob costs a process and a process costs
/// milliseconds (ADR 0021). A file that can contribute nothing is never read —
/// generated content defines nothing (a lockfile would otherwise appear to
/// define half the dependency tree), a binary carries no lines, and a file
/// whose every hunk is a pure deletion has no added line to attribute.
///
/// A gitlink is excluded twice over: there is no blob behind the path, so asking
/// for one is an error rather than an absence, and `build` skips it outright so
/// its pseudo-hunk never reaches the fallback either.
fn parse_files<G: ObjectReader>(
    git: &G,
    head: &str,
    view: &DiffView,
    symbols: &SymbolReaders,
) -> Result<HashMap<usize, FileSymbols>, EngineError> {
    let wanted: Vec<usize> = view
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            f.generated.is_none()
                && !f.binary
                && f.submodule.is_none()
                && f.hunks.iter().any(|&hi| !view.hunks[hi].added.is_empty())
        })
        .map(|(fi, _)| fi)
        .collect();

    let specs: Vec<(&str, &[u8])> = wanted
        .iter()
        .map(|&fi| (head, view.files[fi].path.as_slice()))
        .collect();

    Ok(wanted
        .iter()
        .copied()
        .zip(git.blobs(&specs)?)
        .filter_map(|(fi, blob)| {
            let path = view.files[fi].path.as_slice();
            let content = blob?;
            Some((fi, symbols.of_file(path, &content)?))
        })
        .collect())
}

/// Symbols reach the schema as text. They are identifiers by construction, so
/// this is the display boundary and lossy conversion is the honest answer to
/// bytes that are not.
fn text(sym: &[u8]) -> String {
    String::from_utf8_lossy(sym).into_owned()
}
