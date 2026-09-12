//! The symbol index: which token on which line resolves to which declaration.
//!
//! [`super::graph`] answers "class C2 depends on class C7, via `lookUpName`".
//! That is the question the ORDERING stage asks. It is not the question a
//! reviewer asks, which is "what is this thing on the line in front of me" —
//! and the two are the same extraction, one class apart.
//!
//! So this is built from the same parse, in the same pass. Every fact here was
//! already computed and thrown away inside `graph::build`: the file, the line,
//! the token's columns, and how far a declaration runs.
//!
//! **It never feeds the ordering.** A wrong entry here shows a reader the wrong
//! snippet, which they can see is wrong; it cannot move a group. That is why
//! this could be looser than the graph, and is not:
//!
//! - **A definition can sit on any line of a parsed file**, not only one the
//!   change wrote. The commonest shape of the reviewer's question is a NEW call
//!   to a helper that was already there, and answering it needs the declaration
//!   wherever it sits. It must still be unambiguous — a name declared twice is
//!   absent, the same rule the graph draws edges by, judged over this wider
//!   population.
//! - **A use is recorded only on a line the change WROTE.** Those are the lines
//!   the reviewer is reading, and they are what bounds this: with any
//!   declaration resolvable, recording every mention in every parsed file would
//!   make the index grow with the size of the FILES rather than of the change.
//!
//! What it cannot answer is a name declared in a file the change never touches.
//! Only files with hunks are parsed ([`super::graph`]), so a call into a helper
//! in an untouched file resolves to nothing. That is the honest limit of a tool
//! that reads a diff rather than a repository.

use super::symbols::Site;
use crate::model::DiffView;
use crate::schema;

/// One unambiguous declaration, before it is given an id.
pub(super) struct Definition {
    pub name: Vec<u8>,
    /// Index into `DiffView::files`.
    pub file: usize,
    /// New-side line, counting from 1.
    pub line: u32,
    pub site: Site,
    /// Index into `Partition::classes`, or `None` where the change did not
    /// write this line — the declaration is real, it is simply not part of the
    /// change, and claiming a class for it would be a lie.
    pub class: Option<usize>,
}

/// One token that resolves to a [`Definition`].
pub(super) struct Use {
    /// Index into the `definitions` slice passed alongside.
    pub def: usize,
    pub file: usize,
    pub line: u32,
    pub site: Site,
}

/// Shape resolved definitions and uses into the document's index.
///
/// Ids are positional and document-local, like `h<N>` and `C<N>`, and they are
/// assigned here in location order. The caller walks hash maps, whose order is
/// not stable across runs, and these rows reach a document that is hashed.
pub(super) fn build(
    view: &DiffView,
    mut definitions: Vec<Definition>,
    mut uses: Vec<Use>,
) -> schema::SymbolIndex {
    // **A definition nothing points at is dropped.** The index exists to answer
    // "what is this token", so a declaration no recorded use reaches can never
    // be shown — and once ANY declaration in a touched file is a candidate,
    // most of them are never reached. On the validation corpus this is the
    // difference between 1184 definitions and the handful actually referenced.
    let reached: std::collections::HashSet<usize> = uses.iter().map(|u| u.def).collect();
    let mut order: Vec<usize> = (0..definitions.len())
        .filter(|i| reached.contains(i))
        .collect();
    order.sort_by_key(|&i| {
        let d = &definitions[i];
        (d.file, d.line, d.site.start, d.name.clone())
    });
    // `rank[old index] = new index`, so the uses can be renumbered without
    // searching for their definition again. A dropped definition keeps a slot
    // it never uses: nothing indexes it, because nothing reached it.
    let mut rank = vec![0usize; definitions.len()];
    for (new, &old) in order.iter().enumerate() {
        rank[old] = new;
    }

    let out: Vec<schema::SymbolDef> = order
        .iter()
        .enumerate()
        .map(|(new, &old)| {
            let d = &mut definitions[old];
            schema::SymbolDef {
                id: format!("s{new}"),
                name: text(&d.name),
                file: text(&view.files[d.file].path),
                line: d.line,
                // Zero means the reader could not see an extent — a regex has
                // no tree to ask. The declaring line is then all there is.
                through: if d.site.through == 0 {
                    d.line
                } else {
                    d.site.through
                },
                start: d.site.start,
                end: d.site.end,
                class: d.class.map(|c| format!("C{c}")),
            }
        })
        .collect();

    for u in &mut uses {
        u.def = rank[u.def];
    }
    uses.sort_by_key(|u| (u.def, u.file, u.line, u.site.start));
    let uses = uses
        .into_iter()
        .map(|u| schema::SymbolUse {
            on: out[u.def].id.clone(),
            file: text(&view.files[u.file].path),
            line: u.line,
            start: u.site.start,
            end: u.site.end,
        })
        .collect();

    schema::SymbolIndex {
        definitions: out,
        uses,
    }
}

/// Paths and names reach the schema as text — the same display boundary
/// `graph::text` is. An identifier is one by construction; a path may not be.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
