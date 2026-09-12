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

use super::sites;
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
    /// The same extraction, one class apart: which token resolves to which
    /// declaration ([`super::sites`]). Read by consumers that SHOW a
    /// dependency; never by the ordering stage.
    pub symbols: schema::SymbolIndex,
}

/// What a name is compared within.
///
/// A global name is compared within its namespace — the body of names the
/// reader says it belongs to. `Widget` read from one Rust file is the same
/// `Widget` read from another, and is NOT the `Widget` in a TypeScript file:
/// nothing here parses a monorepo's build graph, so a name shared across two
/// languages is a coincidence the tool cannot tell from a fact (ADR 0031).
///
/// A file-local name is compared within its file, which is narrower than any
/// namespace — `label` in one file and `label` in another are two symbols, and
/// neither can draw an edge to the other's class.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Namespace {
    Language(Vec<u8>),
    File(usize),
}

type Key = (Namespace, Vec<u8>);

fn key(file: usize, namespace: &[u8], symbol: &Symbol) -> Key {
    let within = match symbol.scope {
        Scope::Global => Namespace::Language(namespace.to_vec()),
        Scope::File => Namespace::File(file),
    };
    (within, symbol.name.clone())
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
    // Which class wrote each added line, so a declaration ON one can name it.
    // A declaration the change did not write has no class, and says so.
    let mut line_class: HashMap<(usize, u32), usize> = HashMap::new();

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
                    let at = |s: &Symbol| key(h.file, &fs.namespace, s);
                    defs[ci].extend(fs.defines_at(line).iter().map(at));
                    refs[ci].extend(fs.references_at(line).iter().map(at));
                    line_class.insert((h.file, line), ci);
                }
            }
        }
    }

    // Only symbols defined by exactly ONE class create edges. A symbol two
    // classes define is ambiguous, and this heuristic cannot say which one a
    // reference meant; a precise `Language` (ADR 0015) would resolve it
    // instead of dropping it.
    //
    // A key carries the namespace it is compared within, so the ambiguity is
    // judged there too: two files each declaring `label` locally are not a
    // clash, one file declaring it twice is, and a Rust `Widget` and a
    // TypeScript one never meet to clash at all.
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

    // The index, from the same parse and the same verdict. A definition whose
    // key has no unique definer is dropped here, so it is absent from the index
    // for exactly the reason it draws no edge.
    // **Every declaration in a parsed file, not only the ones the change wrote.**
    //
    // The graph above reads added lines because it asks what the CHANGE
    // introduces. The index is asked a different question — "what is this name
    // on the line in front of me" — and the commonest shape of that question is
    // a new call to a helper that was already there. Answering it needs the
    // declaration wherever it sits.
    //
    // It costs nothing the graph can see: `defs` is untouched above, so edges,
    // the single-definer rule and the corpus figures are exactly as they were.
    let mut parsed_files: Vec<&usize> = parsed.keys().collect();
    parsed_files.sort();
    let mut declared: Vec<(Key, sites::Definition)> = Vec::new();
    for &fi in &parsed_files {
        let fs = &parsed[fi];
        for (i, row) in fs.defines.iter().enumerate() {
            let line = i as u32 + 1;
            for sym in row {
                declared.push((
                    key(*fi, &fs.namespace, sym),
                    sites::Definition {
                        name: sym.name.clone(),
                        file: *fi,
                        line,
                        site: sym.site,
                        // `None` where the change did not write this line: the
                        // declaration is real, it is simply not part of the
                        // change, and claiming a class for it would be a lie.
                        class: line_class.get(&(*fi, line)).copied(),
                    },
                ));
            }
        }
    }

    // The index's OWN single-definer rule, over that wider set. Same rule as
    // the graph's and a different population, so it has to be computed here:
    // a name the change declares once but the file declares twice is ambiguous
    // to a reader even though it is unambiguous to the graph.
    let mut index_definer: HashMap<&Key, Option<usize>> = HashMap::new();
    for (n, (k, _)) in declared.iter().enumerate() {
        index_definer
            .entry(k)
            .and_modify(|e| *e = None)
            .or_insert(Some(n));
    }

    let mut definitions: Vec<sites::Definition> = Vec::new();
    let mut of_key: HashMap<&Key, usize> = HashMap::new();
    for (k, d) in &declared {
        if !matches!(index_definer.get(k), Some(Some(_))) {
            continue;
        }
        // One entry per NAME. A query can capture one declaration twice, and
        // two ids for one declaration would step a reader through the same
        // snippet twice.
        if of_key.contains_key(k) {
            continue;
        }
        of_key.insert(k, definitions.len());
        definitions.push(sites::Definition {
            name: d.name.clone(),
            file: d.file,
            line: d.line,
            site: d.site,
            class: d.class,
        });
    }

    // **Uses come from the lines the change WROTE, and only those.**
    //
    // Those are the lines the reviewer is reading, and they are what bounds
    // this: recording every mention in every parsed file would make the index
    // grow with the SIZE OF THE FILES rather than with the size of the change,
    // now that any declaration can be resolved against.
    //
    // The cost is that a reader who opens context and lands on an older call
    // site gets nothing there. That was the author's call, and it is the right
    // way round: the change is the thing being read.
    let mut uses: Vec<sites::Use> = Vec::new();
    for members in &partition.classes {
        for &hi in members {
            let h = &view.hunks[hi];
            let file = view.file_of(h);
            if file.generated.is_some() || file.submodule.is_some() {
                continue;
            }
            let Some(fs) = parsed.get(&h.file) else {
                continue;
            };
            for i in 0..h.added.len() {
                let line = h.new_start + i as u32;
                for sym in fs.references_at(line) {
                    let k = key(h.file, &fs.namespace, sym);
                    let Some(&def) = of_key.get(&k) else { continue };
                    // A declaration is not a use of itself. The crude reader
                    // has no veto — its reference regex takes every identifier
                    // on a line, the name it just declared included — so
                    // `fn helper()` reports `helper` as reading `helper`.
                    // Pointing a reader at the line they are standing on is the
                    // one answer never worth giving.
                    //
                    // Position, not name: the same name genuinely used again on
                    // its own declaring line — a default argument, a one-line
                    // recursive call — is a real use and stays.
                    let d = &definitions[def];
                    if d.file == h.file && d.line == line && d.site.start == sym.site.start {
                        continue;
                    }
                    uses.push(sites::Use {
                        def,
                        file: h.file,
                        line,
                        site: sym.site,
                    });
                }
            }
        }
    }

    let symbols = sites::build(definitions, uses);

    Ok(ClassGraph {
        symbols,
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
